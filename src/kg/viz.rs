//! Offline, local code-graph visualization.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::store::{Graph, RefRow, SymbolRow};
use crate::context::AppContext;

const MAX_REFERENCE_ROWS: usize = 60_000;

#[derive(Debug, Clone, Serialize)]
pub struct GraphVizReport {
    pub html: String,
    pub symbols: usize,
    pub edges: usize,
    pub omitted_symbols: usize,
    pub reference_rows_considered: usize,
    pub index_update: Option<IndexUpdate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexUpdate {
    /// Why `kg viz` refreshed the graph.
    pub reason: String,
    /// Whether refresh required a full rebuild.
    pub full_rebuild: bool,
    /// Updated graph totals.
    pub files: usize,
    pub symbols: usize,
    pub references: usize,
    /// Time spent indexing, in milliseconds.
    pub elapsed_ms: u64,
}

#[derive(Serialize)]
struct VizNode {
    name: String,
    kind: String,
    path: String,
    line: usize,
    exported: bool,
    degree: usize,
}

#[derive(Serialize)]
struct VizEdge {
    source: usize,
    target: usize,
}

#[derive(Serialize)]
struct VizData {
    workspace: String,
    built_at: String,
    total_symbols: usize,
    total_references: usize,
    nodes: Vec<VizNode>,
    edges: Vec<VizEdge>,
}

/// Refresh the graph when needed, generate the viewer, and open it in the browser.
pub fn viz(ctx: &AppContext, limit: usize) -> Result<GraphVizReport> {
    let status = super::status(ctx)?;
    let root_mismatch = status.indexed && status.root != super::root_label(&ctx.root);
    let refresh_reason = if !status.indexed {
        Some(if ctx.graph_path.is_file() {
            "the existing index is missing or incompatible".to_string()
        } else {
            "no index exists yet".to_string()
        })
    } else if root_mismatch {
        Some("the index belongs to a different workspace".to_string())
    } else if status.stale_files > 0 {
        Some(format!("{} workspace files changed", status.stale_files))
    } else {
        None
    };

    let index_update = if let Some(reason) = refresh_reason {
        if !ctx.quiet && !ctx.json {
            eprintln!("kg viz: {reason}; indexing this workspace locally");
        }
        let mut progress = |line: &str| {
            if !ctx.quiet && !ctx.json {
                eprintln!("kg viz: {line}");
            }
        };
        let report = super::index(ctx, false, true, &mut progress)?;
        Some(IndexUpdate {
            reason,
            full_rebuild: report.full,
            files: report.files,
            symbols: report.symbols,
            references: report.references,
            elapsed_ms: report.elapsed_ms,
        })
    } else {
        None
    };

    let graph = Graph::open_readonly(&ctx.graph_path)?;
    if !graph.schema_present() {
        bail!("The code graph index is still incompatible after automatic refresh.");
    }

    let all_symbols = graph.all_symbols()?;
    if all_symbols.is_empty() {
        bail!("The index found no declarations in this workspace.");
    }
    let all_refs = graph.visualization_refs(MAX_REFERENCE_ROWS)?;
    let data = make_data(ctx, &graph, all_symbols, all_refs, limit)?;
    let symbols = data.nodes.len();
    let edges = data.edges.len();
    let omitted_symbols = data.total_symbols.saturating_sub(symbols);
    let reference_rows_considered = data.total_references;
    let html = render_html(&data)?;
    let output = ctx.root.join(".memnoth").join("graph.html");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&output, html.as_bytes())
        .with_context(|| format!("cannot write {}", output.display()))?;

    if !ctx.json {
        let url = file_url(&output)?;
        crate::auth::open_browser(&url).context("could not open the graph in your browser")?;
    }

    Ok(GraphVizReport {
        html: output.display().to_string(),
        symbols,
        edges,
        omitted_symbols,
        reference_rows_considered,
        index_update,
    })
}

fn make_data(
    ctx: &AppContext,
    graph: &Graph,
    symbols: Vec<SymbolRow>,
    refs: Vec<RefRow>,
    limit: usize,
) -> Result<VizData> {
    let requested_limit = limit.clamp(100, 6000);
    let total_symbols = symbols.len();
    let total_references = refs.len();

    let mut by_file_name: HashMap<(String, String), Vec<usize>> = HashMap::new();
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (id, symbol) in symbols.iter().enumerate() {
        by_file_name
            .entry((symbol.path.clone(), symbol.name.clone()))
            .or_default()
            .push(id);
        by_name.entry(symbol.name.clone()).or_default().push(id);
    }

    let mut edges_seen = HashSet::new();
    let mut degrees = vec![0usize; symbols.len()];
    for reference in &refs {
        let Some(container) = reference.container.as_deref() else {
            continue;
        };
        let source_id = by_file_name
            .get(&(reference.path.clone(), container.to_string()))
            .and_then(|candidates| {
                candidates
                    .iter()
                    .copied()
                    .filter(|id| symbols[*id].line <= reference.line)
                    .max_by_key(|id| symbols[*id].line)
            });
        let Some(source_id) = source_id else { continue };
        let Some(targets) = by_name.get(&reference.name) else {
            continue;
        };
        // Prefer a declaration in the same file. Otherwise only draw a link
        // when the name resolves to one declaration; duplicate names stay
        // unconnected instead of implying a certainty the index does not have.
        let target_id = by_file_name
            .get(&(reference.path.clone(), reference.name.clone()))
            .and_then(|same_file| same_file.first().copied())
            .or_else(|| (targets.len() == 1).then_some(targets[0]));
        let Some(target_id) = target_id else { continue };
        if source_id != target_id && edges_seen.insert((source_id, target_id)) {
            degrees[source_id] += 1;
            degrees[target_id] += 1;
        }
    }

    let mut selected: Vec<usize> = (0..symbols.len()).collect();
    selected.sort_by(|a, b| {
        degrees[*b]
            .cmp(&degrees[*a])
            .then_with(|| symbols[*a].path.cmp(&symbols[*b].path))
            .then_with(|| symbols[*a].line.cmp(&symbols[*b].line))
    });
    selected.truncate(requested_limit);
    let selected_set: HashSet<usize> = selected.iter().copied().collect();
    selected.sort_unstable();

    let viz_nodes: Vec<VizNode> = selected
        .iter()
        .map(|id| {
            let symbol = &symbols[*id];
            VizNode {
                name: symbol.name.clone(),
                kind: symbol.kind.clone(),
                path: symbol.path.clone(),
                line: symbol.line,
                exported: symbol.exported,
                degree: degrees[*id],
            }
        })
        .collect();
    let position_by_id: HashMap<usize, usize> = selected
        .iter()
        .enumerate()
        .map(|(i, old)| (*old, i))
        .collect();

    let mut viz_edges = Vec::new();
    for (source, target) in edges_seen {
        if selected_set.contains(&source) && selected_set.contains(&target) {
            viz_edges.push(VizEdge {
                source: position_by_id[&source],
                target: position_by_id[&target],
            });
        }
    }
    viz_edges.sort_by_key(|edge| (edge.source, edge.target));

    let counts = graph.counts()?;
    let built_at = graph
        .get_meta("built_at_ms")?
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown time".to_string());
    Ok(VizData {
        workspace: ctx.root.display().to_string(),
        built_at,
        total_symbols: total_symbols.min(counts.symbols),
        total_references,
        nodes: viz_nodes,
        edges: viz_edges,
    })
}

fn render_html(data: &VizData) -> Result<String> {
    let mut json = serde_json::to_string(data)?;
    json = json
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(HTML.replace("{graph_data}", &json))
}

fn file_url(path: &Path) -> Result<String> {
    let absolute = std::fs::canonicalize(path)
        .with_context(|| format!("cannot resolve {}", path.display()))?;
    let path = absolute.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    let prefix = if encoded.starts_with('/') {
        "file://"
    } else {
        "file:///"
    };
    Ok(format!("{prefix}{encoded}"))
}

const HTML: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Memnoth · Code Graph</title>
<style>
:root{color-scheme:dark;--bg:#0b0e12;--panel:#10151b;--line:#202832;--muted:#82909e;--text:#e7edf2;--mint:#75e2c3;--amber:#ffb86b;--blue:#80bfff;--pink:#e9a6d5}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:14px/1.45 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;overflow:hidden}
header{height:64px;display:flex;align-items:center;gap:18px;padding:0 22px;border-bottom:1px solid var(--line);background:#0d1116}
.brand{font-weight:720;font-size:16px;letter-spacing:.01em}.brand i{font-style:normal;color:var(--mint)}.crumb{color:var(--muted)}.stats{margin-left:auto;color:#abb6c1;font-variant-numeric:tabular-nums}.stats b{color:var(--text);font-weight:600}.workspace{max-width:35vw;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--muted)}
main{height:calc(100vh - 64px);display:grid;grid-template-columns:minmax(0,1fr) 310px}.stage{position:relative;min-width:0;overflow:hidden;background:radial-gradient(ellipse at 50% 45%,#111a20 0%,#0b0e12 72%)}canvas{display:block;width:100%;height:100%;cursor:grab}canvas.drag{cursor:grabbing}
.toolbar{position:absolute;top:18px;left:20px;display:flex;gap:8px;align-items:center}.search{width:min(360px,40vw);height:40px;background:#111820;border:1px solid #2b3742;border-radius:9px;color:var(--text);padding:0 13px;font:inherit;outline:none}.search:focus{border-color:#529b87;box-shadow:0 0 0 3px #75e2c31c}.button{height:38px;padding:0 12px;background:#111820;border:1px solid #2b3742;border-radius:8px;color:#c5d0d9;font:inherit;cursor:pointer}.button:hover,.button.on{border-color:#4b8e7d;color:var(--mint)}
.legend{position:absolute;left:20px;bottom:17px;display:flex;gap:14px;flex-wrap:wrap;color:var(--muted);font-size:12px}.legend span{display:flex;align-items:center;gap:6px}.dot{width:8px;height:8px;border-radius:50%;display:inline-block}
aside{min-width:0;border-left:1px solid var(--line);background:var(--panel);display:flex;flex-direction:column}.side-head{padding:20px 18px 14px;border-bottom:1px solid var(--line)}.eyebrow{text-transform:uppercase;letter-spacing:.12em;font-size:10px;color:var(--muted);font-weight:700}.side-head h1{margin:5px 0 0;font-size:16px;font-weight:650}.filters{padding:14px 18px;border-bottom:1px solid var(--line);display:flex;gap:8px;align-items:center}.filters select{width:100%;height:36px;background:#0d1217;border:1px solid #2a333d;border-radius:7px;color:#cbd4dc;padding:0 9px;font:inherit}.inspector{padding:18px;overflow:auto;flex:1}.empty{margin-top:32px;text-align:center;color:var(--muted);font-size:13px}.empty .glyph{font-size:28px;color:#364a50;margin-bottom:9px}.name{font-size:19px;font-weight:680;line-height:1.25;overflow-wrap:anywhere}.kind{margin-top:7px;color:var(--mint);font-size:12px}.path{margin-top:14px;color:#aab5be;font:12px/1.55 ui-monospace,SFMono-Regular,Consolas,monospace;overflow-wrap:anywhere}.metrics{display:flex;gap:8px;margin-top:15px}.metric{flex:1;padding:9px;background:#0c1116;border:1px solid #202832;border-radius:7px;color:var(--muted);font-size:11px}.metric b{display:block;color:var(--text);font-size:15px;font-weight:600}.section{margin-top:22px}.section h2{margin:0 0 8px;color:var(--muted);font-size:10px;text-transform:uppercase;letter-spacing:.11em}.neighbor{display:block;width:100%;padding:8px 9px;margin:4px 0;text-align:left;background:#0d1217;border:1px solid transparent;border-radius:6px;color:#cbd4dc;font:12px ui-monospace,monospace;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;cursor:pointer}.neighbor:hover{border-color:#33423f;color:var(--mint)}.foot{padding:13px 18px;border-top:1px solid var(--line);color:var(--muted);font-size:11px}.foot kbd{font:inherit;color:#cad5de;background:#1b232b;border-radius:4px;padding:2px 5px}.notice{position:absolute;right:18px;bottom:16px;color:var(--muted);font-size:11px}.notice strong{color:var(--amber);font-weight:550}
@media(max-width:760px){header{height:54px;padding:0 14px}.workspace,.crumb{display:none}.stats{font-size:12px}main{height:calc(100vh - 54px);grid-template-columns:minmax(0,1fr)}aside{position:absolute;right:0;top:54px;bottom:0;width:min(300px,82vw);transform:translateX(100%);transition:transform .18s ease;box-shadow:-15px 0 50px #0008}aside.open{transform:translateX(0)}.toolbar{top:12px;left:12px}.search{width:min(250px,55vw)}.legend{left:12px;bottom:11px}.notice{display:none}}
</style></head><body>
<header><div class="brand"><i>◈</i> memnoth</div><div class="crumb">/</div><div>Code graph</div><div class="stats"><b id="stat-nodes">—</b> symbols <span style="color:#43505b">·</span> <b id="stat-edges">—</b> mention links</div><div class="workspace" id="workspace"></div></header>
<main><section class="stage" id="stage"><canvas id="graph"></canvas><div class="toolbar"><input id="search" class="search" placeholder="Find a symbol or file…  /" autocomplete="off"><button class="button" id="fit" title="Fit graph (F)">Fit</button><button class="button" id="focus" title="Show selected symbol's neighborhood">Focus</button><button class="button" id="details" title="Toggle details on small screens">Details</button></div><div class="legend" id="legend"></div><div class="notice" id="notice"></div></section>
<aside id="aside"><div class="side-head"><div class="eyebrow">Graph inspector</div><h1 id="inspector-title">Explore the workspace</h1></div><div class="filters"><select id="kind"><option value="">All symbol kinds</option></select></div><div class="inspector" id="inspector"><div class="empty"><div class="glyph">◌</div>Select a node to inspect its<br>definition and nearby mentions.</div></div><div class="foot"><kbd>/</kbd> search　<kbd>F</kbd> fit　scroll zoom　drag canvas</div></aside></main>
<script>
const DATA={graph_data};
const canvas=document.getElementById('graph'),ctx=canvas.getContext('2d'),stage=document.getElementById('stage');
const nodes=DATA.nodes.map((n,i)=>({...n,id:i,x:0,y:0,screenX:0,screenY:0})),edges=DATA.edges;
document.getElementById('workspace').textContent=DATA.workspace;
document.getElementById('stat-nodes').textContent=nodes.length.toLocaleString();document.getElementById('stat-edges').textContent=edges.length.toLocaleString();
const kinds=[...new Set(nodes.map(n=>n.kind))].sort();for(const kind of kinds){const o=document.createElement('option');o.value=kind;o.textContent=kind;document.getElementById('kind').append(o)}
const colors=['#75e2c3','#80bfff','#ffb86b','#e9a6d5','#b7a3ff','#e5d477','#71c9cf','#d58f8f'];const kindColor=Object.fromEntries(kinds.map((k,i)=>[k,colors[i%colors.length]]));
document.getElementById('legend').innerHTML=kinds.slice(0,6).map(k=>'<span><i class="dot" style="background:'+kindColor[k]+'"></i>'+escapeHtml(k)+'</span>').join('');
const dirs=[...new Set(nodes.map(n=>n.path.split('/')[0]))].sort();const clusterIndex=new Map(dirs.map((d,i)=>[d,i])),clusterCenters=[];
function hash(s){let h=2166136261;for(let i=0;i<s.length;i++)h=Math.imul(h^s.charCodeAt(i),16777619);return h>>>0}
function layout(){const cx=0,cy=0,R=Math.max(320,Math.min(nodes.length*5,1050));const groups=new Map(dirs.map(d=>[d,[]]));for(const n of nodes)groups.get(n.path.split('/')[0]).push(n);for(const [dir,list] of groups){const ci=clusterIndex.get(dir),a=2*Math.PI*ci/Math.max(1,dirs.length)-Math.PI/2;const gx=cx+Math.cos(a)*R*.62,gy=cy+Math.sin(a)*R*.62;clusterCenters.push({label:dir,x:gx,y:gy});const ring=Math.max(28,Math.sqrt(list.length)*12);for(let j=0;j<list.length;j++){const n=list[j],seed=hash(n.path+n.name+n.line),t=(j/Math.max(1,list.length))*Math.PI*2+(seed%1000)/1000;const rr=ring*(.55+(seed%100)/220);n.x=gx+Math.cos(t)*rr;n.y=gy+Math.sin(t)*rr}}}
layout();let dpr=1,w=0,h=0,scale=1,tx=0,ty=0,selected=-1,hover=-1,query='',kindFilter='',focusOnly=false,drag=null;
function resize(){const r=stage.getBoundingClientRect();dpr=Math.min(2,devicePixelRatio||1);w=r.width;h=r.height;canvas.width=Math.round(w*dpr);canvas.height=Math.round(h*dpr);ctx.setTransform(dpr,0,0,dpr,0,0);fit();draw()}new ResizeObserver(resize).observe(stage);
function fit(){const xs=nodes.map(n=>n.x),ys=nodes.map(n=>n.y),pad=90;const minX=Math.min(...xs),maxX=Math.max(...xs),minY=Math.min(...ys),maxY=Math.max(...ys);scale=Math.min((w-pad*2)/Math.max(1,maxX-minX),(h-pad*2)/Math.max(1,maxY-minY),1.2);tx=(w-(minX+maxX)*scale)/2;ty=(h-(minY+maxY)*scale)/2}
function visible(n){return(!kindFilter||n.kind===kindFilter)&&(!focusOnly||selected<0||n.id===selected||adjacent.has(n.id))&&(!query||`${n.name} ${n.path}`.toLowerCase().includes(query))}
let adjacent=new Set();function neighbors(id){adjacent=new Set([id]);for(const e of edges)if(e.source===id)adjacent.add(e.target);else if(e.target===id)adjacent.add(e.source)}
function draw(){if(!w||!h)return;ctx.clearRect(0,0,w,h);ctx.save();ctx.translate(tx,ty);ctx.scale(scale,scale);for(const n of nodes){n.screenX=tx+n.x*scale;n.screenY=ty+n.y*scale}
const active=selected>=0||query||kindFilter||focusOnly;ctx.lineWidth=1/scale;for(const e of edges){const a=nodes[e.source],b=nodes[e.target];if(active&&!((visible(a)&&visible(b))||(selected>=0&&(a.id===selected||b.id===selected))))continue;const near=selected>=0&&(a.id===selected||b.id===selected);ctx.strokeStyle=near?'#75e2c36e':'#34414b45';ctx.beginPath();ctx.moveTo(a.x,a.y);ctx.lineTo(b.x,b.y);ctx.stroke()}
for(const n of nodes){const vis=visible(n),related=selected>=0&&(n.id===selected||adjacent.has(n.id));if(active&&!vis&&!related)continue;const radius=Math.min(7,2.2+Math.log2(1+n.degree)*.7);ctx.globalAlpha=active&&!vis?.12:related?.98:.78;ctx.fillStyle=kindColor[n.kind]||'#91a0ad';ctx.beginPath();ctx.arc(n.x,n.y,related?radius+1.7:radius,0,Math.PI*2);ctx.fill();if(n.id===selected||n.name.toLowerCase()===query){ctx.globalAlpha=1;ctx.strokeStyle=n.id===selected?'#fff':'#ffb86b';ctx.lineWidth=1.5/scale;ctx.beginPath();ctx.arc(n.x,n.y,radius+4,0,Math.PI*2);ctx.stroke()}}
ctx.globalAlpha=.65;ctx.fillStyle='#8d9ba5';ctx.font=`${11/scale}px ui-sans-serif,system-ui`;for(const c of clusterCenters)ctx.fillText(c.label,c.x,c.y-35);
ctx.restore();}
function escapeHtml(s){return String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
function select(id){selected=id;neighbors(id);focusOnly=false;const n=nodes[id];document.getElementById('inspector-title').textContent='Symbol details';const near=edges.filter(e=>e.source===id||e.target===id);const links=near.slice(0,18).map(e=>{const other=nodes[e.source===id?e.target:e.source];return `<button class="neighbor" data-id="${other.id}">${escapeHtml(e.source===id?'mentions → ':'← mentioned by ')}${escapeHtml(other.name)}</button>`}).join('');document.getElementById('inspector').innerHTML=`<div class="name">${escapeHtml(n.name)}</div><div class="kind">${escapeHtml(n.kind)}${n.exported?' · exported':''}</div><div class="path">${escapeHtml(n.path)}:${n.line}</div><div class="metrics"><div class="metric"><b>${n.degree}</b>connections</div><div class="metric"><b>${near.length}</b>visible links</div></div><div class="section"><h2>Nearby mentions</h2>${links||'<div style="color:#82909e;font-size:12px">No linked declarations in this view.</div>'}</div><div class="foot" style="padding:12px 0 0;border:0">Index snapshot · ${escapeHtml(DATA.built_at)}</div>`;document.querySelectorAll('.neighbor').forEach(b=>b.onclick=()=>select(+b.dataset.id));draw()}
canvas.addEventListener('pointerdown',e=>{const x=e.offsetX,y=e.offsetY;let hit=-1,dist=14;for(const n of nodes){if(!visible(n))continue;const d=Math.hypot(n.screenX-x,n.screenY-y);if(d<dist){hit=n.id;dist=d}}if(hit>=0){select(hit);drag={id:hit,px:e.clientX,py:e.clientY,moved:false};canvas.setPointerCapture(e.pointerId)}else{drag={pan:true,px:e.clientX,py:e.clientY};canvas.setPointerCapture(e.pointerId);canvas.classList.add('drag')}});
canvas.addEventListener('pointermove',e=>{if(drag){const dx=e.clientX-drag.px,dy=e.clientY-drag.py;if(drag.pan){tx+=dx;ty+=dy}else if(drag.id!==undefined){const n=nodes[drag.id];n.x+=dx/scale;n.y+=dy/scale;drag.moved=true}drag.px=e.clientX;drag.py=e.clientY;draw();return}let id=-1,best=14;for(const n of nodes){const d=Math.hypot(n.screenX-e.offsetX,n.screenY-e.offsetY);if(d<best){best=d;id=n.id}}if(id!==hover){hover=id;canvas.title=id>=0?`${nodes[id].name} · ${nodes[id].path}:${nodes[id].line}`:''}});
canvas.addEventListener('pointerup',()=>{drag=null;canvas.classList.remove('drag')});canvas.addEventListener('pointercancel',()=>{drag=null;canvas.classList.remove('drag')});canvas.addEventListener('wheel',e=>{e.preventDefault();const old=scale,factor=e.deltaY<0?1.12:.89;scale=Math.max(.08,Math.min(4,scale*factor));tx=e.offsetX-(e.offsetX-tx)*scale/old;ty=e.offsetY-(e.offsetY-ty)*scale/old;draw()},{passive:false});
const search=document.getElementById('search');search.addEventListener('input',()=>{query=search.value.trim().toLowerCase();draw();if(query){const found=nodes.find(n=>n.name.toLowerCase()===query)||nodes.find(n=>`${n.name} ${n.path}`.toLowerCase().includes(query));if(found){selected=found.id;neighbors(found.id);tx=w/2-found.x*scale;ty=h/2-found.y*scale;show(found.id);draw()}}});function show(id){const n=nodes[id];document.getElementById('inspector-title').textContent='Search match';document.getElementById('inspector').innerHTML=`<button class="neighbor" data-id="${id}">${escapeHtml(n.name)} · ${escapeHtml(n.kind)}</button><div class="path">${escapeHtml(n.path)}:${n.line}</div>`;document.querySelector('.neighbor').onclick=()=>select(id)}
document.getElementById('kind').onchange=e=>{kindFilter=e.target.value;draw()};document.getElementById('fit').onclick=()=>{fit();draw()};document.getElementById('focus').onclick=e=>{focusOnly=!focusOnly;if(selected>=0)neighbors(selected);e.currentTarget.classList.toggle('on',focusOnly);draw()};document.getElementById('details').onclick=()=>document.getElementById('aside').classList.toggle('open');
document.addEventListener('keydown',e=>{if(e.key==='/'&&document.activeElement!==search){e.preventDefault();search.focus()}else if(e.key==='Escape'){search.value='';query='';search.blur();draw()}else if(e.key.toLowerCase()==='f'&&document.activeElement!==search){fit();draw()}});
const notice=document.getElementById('notice');notice.innerHTML=`Index ${escapeHtml(DATA.built_at)}${DATA.total_symbols>nodes.length?` · <strong>${(DATA.total_symbols-nodes.length).toLocaleString()} lower-connected symbols omitted</strong>`:''}${DATA.total_references>=60000?' · link sample capped':''}`;
if(nodes.length){const best=nodes.reduce((a,b)=>b.degree>a.degree?b:a,nodes[0]);document.getElementById('inspector-title').textContent='Explore the workspace';document.getElementById('inspector').innerHTML=`<div class="empty"><div class="glyph">◌</div>${nodes.length.toLocaleString()} symbols arranged by source folder.<br>Search, filter, then select a node.</div><div class="section"><h2>Most connected</h2><button class="neighbor" id="hub">${escapeHtml(best.name)} · ${best.degree} links</button></div><div class="section"><h2>Snapshot</h2><div class="path">${escapeHtml(DATA.total_symbols.toLocaleString())} declarations<br>${escapeHtml(DATA.total_references.toLocaleString())} mention rows sampled<br>${escapeHtml(DATA.built_at)}</div></div>`;document.getElementById('hub').onclick=()=>select(best.id)}
</script></body></html>"##;
