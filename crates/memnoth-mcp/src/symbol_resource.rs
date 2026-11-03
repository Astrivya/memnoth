//! `kg://symbol/{qname}` — the code graph as a resource a host can read.
//!
//! Kept out of `lib.rs` for the same reason every other subsystem is: the server's module is
//! already the longest file in the crate, and a reader looking for "what does the symbol resource
//! answer" should find it rather than a match arm.

use std::path::Path;

use memnoth_codegraph::open_store;
use serde_json::{Value, json};

use crate::{READ_CACHE_TTL_MS, resource_not_found, workspace_root_for_vault};

/// The URI template, declared beside the reader so the two cannot drift apart.
pub(crate) fn template() -> Value {
    json!({
        "uriTemplate": "kg://symbol/{qname}",
        "name": "Symbol page",
        "description": "One symbol from the code graph: kind, file, line span, signature, body source and its direct callers",
        "mimeType": "application/json",
    })
}

/// One symbol, as the graph and the file agree on it.
///
/// The source comes from the file the graph points at, not from a copy in the database: the
/// graph stores spans, so a read that returned stale text after an edit would be reporting a
/// body that no longer exists. Callers are the one-hop inbound `calls` edges, which is what a
/// reader needs to know whether changing this is safe.
pub(crate) fn read(uri: &str, vault: Option<&Path>) -> std::result::Result<(Value, i64), Value> {
    let root = workspace_root_for_vault(vault).ok_or_else(|| {
        resource_not_found(
            "this server is not serving a workspace vault, so it has no code graph to read".into(),
        )
    })?;
    let qname = percent_decode(&uri["kg://symbol/".len()..]);
    if qname.trim().is_empty() {
        return Err(resource_not_found(
            "kg://symbol/ needs a symbol: kg://symbol/crate::module::name".into(),
        ));
    }
    let db = root.join(".memnoth").join("kg.db");
    if !db.exists() {
        return Err(resource_not_found(format!(
            "no code graph at {} — run kg_index for this workspace first",
            db.display()
        )));
    }
    let store = open_store(&db).map_err(|e| resource_not_found(e.to_string()))?;
    let snippet = memnoth_codegraph::snippet(&store, &root, &qname)
        .map_err(|e| resource_not_found(e.to_string()))?
        .ok_or_else(|| resource_not_found(format!("{qname} is not in this graph")))?;
    let impact = memnoth_codegraph::blast_radius(&store, &qname, 1, 20, false)
        .map_err(|e| resource_not_found(e.to_string()))?;
    let callers: Vec<Value> = impact
        .direct
        .iter()
        .map(|caller| {
            json!({
                "qname": caller.qname,
                "kind": caller.kind,
                "rel_path": caller.rel_path,
                "line": caller.line,
            })
        })
        .collect();
    let body = json!({
        "qname": snippet.qname,
        "rel_path": snippet.rel_path,
        "start_line": snippet.start_line,
        "end_line": snippet.end_line,
        "signature": snippet.signature,
        "source": snippet.source,
        "callers": callers,
        "callers_total": impact.direct.len(),
        "generation": impact.generation,
    });
    Ok((body, READ_CACHE_TTL_MS))
}

/// `%3A%3A` → `::`. Hosts percent-encode a URI template's variable, and a qname is nothing but
/// colons; decoding by hand keeps this crate free of a dependency for fifteen lines of work.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            match u8::from_str_radix(hex, 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                Err(_) => {
                    // A stray `%` is a literal: the template is typed by hand often enough that
                    // failing the whole read over it would be hostile.
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_percent_encoded_qname_decodes() {
        assert_eq!(percent_decode("demo%3A%3Alib%3A%3Arun"), "demo::lib::run");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn a_stray_percent_is_a_literal_not_a_failure() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
    }
}
