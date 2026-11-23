//! ALG19 — compact graph storage experiments (feature-gated, A/B-checked).
//!
//! The module under test is included by path so these tests run without wiring
//! the new module into `lib.rs` or declaring the Cargo feature. The exact wiring
//! diff is in the hand-off report. Every entry point refuses to change the
//! default `graph.db` format; enabling requires both the runtime flag and a
//! fixture compatibility check.

// The path-included module exposes a wider API than any single test uses;
// dead_code is expected here. Production wiring is behind a Cargo feature.
#[allow(dead_code)]
#[path = "../src/graph_compression.rs"]
mod graph_compression;

use std::collections::HashSet;

use graph_compression::{
    CompactConfig, CompactEdge, CompactIndex, CompactNode, CompactStore, EnableError, MmapView,
    NodeId, ScopeId, compact_round_trip, decode_snapshot, diff_snapshots, encode_snapshot,
    sidecar_path, snapshot_from_store, try_enable,
};
use graph_compression::{GraphSnapshot, apply_delta};

fn sample_graph() -> GraphSnapshot {
    GraphSnapshot {
        nodes: vec![
            CompactNode {
                id: NodeId(0),
                scope: ScopeId(0),
                qname: "src/a.rs::alpha".into(),
                kind: "function".into(),
            },
            CompactNode {
                id: NodeId(1),
                scope: ScopeId(0),
                qname: "src/a.rs::beta".into(),
                kind: "function".into(),
            },
            CompactNode {
                id: NodeId(2),
                scope: ScopeId(1),
                qname: "src/b.rs::gamma".into(),
                kind: "struct".into(),
            },
            CompactNode {
                id: NodeId(3),
                scope: ScopeId(1),
                qname: "src/b.rs::delta".into(),
                kind: "function".into(),
            },
        ],
        edges: vec![
            CompactEdge {
                src: NodeId(0),
                dst: NodeId(1),
                kind: "calls".into(),
            },
            CompactEdge {
                src: NodeId(0),
                dst: NodeId(2),
                kind: "calls".into(),
            },
            CompactEdge {
                src: NodeId(2),
                dst: NodeId(3),
                kind: "calls".into(),
            },
        ],
        generation: 7,
    }
}

#[test]
fn partition_isolation() {
    let graph = sample_graph();
    let partitions = graph.partition_by_scope();

    assert_eq!(partitions.len(), 2, "two scopes should make two partitions");
    assert_eq!(
        partitions.get(&ScopeId(0)).unwrap(),
        &vec![NodeId(0), NodeId(1)]
    );
    assert_eq!(
        partitions.get(&ScopeId(1)).unwrap(),
        &vec![NodeId(2), NodeId(3)]
    );

    // Isolation invariant: a node id belongs to exactly one partition.
    let mut seen = HashSet::new();
    for ids in partitions.values() {
        for id in ids {
            assert!(seen.insert(*id), "node {id:?} appeared in two partitions");
        }
    }
    assert_eq!(seen.len(), graph.nodes.len());

    // Partitions are owned: editing one cannot leak into another.
    let mut edited = partitions.clone();
    edited.get_mut(&ScopeId(0)).unwrap().push(NodeId(99));
    assert_eq!(edited.get(&ScopeId(0)).unwrap().len(), 3);
    assert_eq!(partitions.get(&ScopeId(1)).unwrap().len(), 2);
    assert_eq!(partitions.get(&ScopeId(0)).unwrap().len(), 2);
}

#[test]
fn compact_index_round_trip() {
    let graph = sample_graph();
    let index: CompactIndex = graph.compact_index();

    assert_eq!(index.len(), 4);
    assert!(!index.is_empty());
    assert_eq!(std::mem::size_of::<NodeId>(), 4, "ids must be compact u32");
    assert_eq!(
        std::mem::size_of::<ScopeId>(),
        2,
        "scopes must be compact u16"
    );

    for node in &graph.nodes {
        assert_eq!(index.get(&node.qname), Some(node.id));
        assert_eq!(index.id_to_qname(node.id), Some(node.qname.as_str()));
    }
    assert_eq!(index.get("src/a.rs::missing"), None);

    // Serialize the index, map it, and read it back with the same results.
    let view = MmapView::from_bytes(index.to_bytes());
    let restored = CompactIndex::from_mmap(&view).expect("decode index");
    assert_eq!(restored, index);
    for node in &graph.nodes {
        assert_eq!(restored.get(&node.qname), Some(node.id));
    }
}

#[test]
fn delta_apply_reproduces_full_graph() {
    let base = sample_graph();
    let mut target = base.clone();
    target.generation = 8;
    target.nodes.retain(|node| node.id != NodeId(3));
    target.nodes.push(CompactNode {
        id: NodeId(4),
        scope: ScopeId(0),
        qname: "src/a.rs::epsilon".into(),
        kind: "function".into(),
    });
    target
        .edges
        .retain(|edge| !(edge.src == NodeId(0) && edge.dst == NodeId(2)));
    target.edges.push(CompactEdge {
        src: NodeId(1),
        dst: NodeId(4),
        kind: "calls".into(),
    });
    target.edges.sort();

    let delta = diff_snapshots(&base, &target);
    assert_eq!(delta.base_generation, 7);
    assert_eq!(delta.target_generation, 8);
    assert_eq!(delta.removed_node_ids, vec![NodeId(3)]);
    assert_eq!(delta.added_nodes.len(), 1);
    assert_eq!(delta.added_nodes[0].id, NodeId(4));

    let applied = apply_delta(&base, &delta).expect("apply delta");
    assert_eq!(
        applied, target,
        "delta replay must reproduce the full graph"
    );

    // A delta stamped for a different base generation is refused.
    let stale = apply_delta(&GraphSnapshot::default(), &delta);
    assert!(stale.is_err());
}

#[test]
fn mismatch_refuses_enable() {
    let graph = sample_graph();
    let mut fixture = graph.clone();
    fixture.nodes[0].qname = "src/a.rs::CHANGED".into();

    let dir = tempfile::tempdir().expect("tempdir");
    let sidecar = dir.path().join("graph.db.compact");

    let error = try_enable(&CompactConfig::on(), &graph, &fixture, &sidecar)
        .expect_err("a mismatched fixture must refuse enable");
    match error {
        EnableError::Incompatible(report) => {
            assert!(
                report.round_trip_identical,
                "the codec itself round-tripped"
            );
            assert!(!report.matches_fixture, "fixture disagrees with store");
            assert!(!report.is_compatible());
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
    assert!(
        !sidecar.exists(),
        "a refused enable must not write a sidecar"
    );

    // The same graph against a matching fixture is accepted.
    let enabled: Result<CompactStore, EnableError> =
        try_enable(&CompactConfig::on(), &graph, &graph, &sidecar);
    assert!(enabled.is_ok(), "matching fixture enables: {enabled:?}");
}

#[test]
fn default_config_does_not_change_on_disk_format() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_path = dir.path().join("graph.db");
    let sidecar = sidecar_path(&store_path);
    assert_ne!(sidecar, store_path, "compact output is a separate sidecar");

    // Build a real, shipped-format SQLite graph with the existing store.
    {
        let store = memnoth_codegraph::Store::open(&store_path).expect("open existing store");
        store
            .conn()
            .execute_batch(
                "INSERT INTO symbols (qname, kind, file_path, span_start, span_end) \
                 VALUES ('src/a.rs::alpha', 'function', 'src/a.rs', 1, 2);",
            )
            .expect("insert symbol into shipped store");
    }
    let before = std::fs::read(&store_path).expect("read store bytes");

    // Default config is the off switch: no compact store, no writes.
    assert!(!CompactConfig::default().enabled);
    let empty = GraphSnapshot::default();
    let refused = try_enable(&CompactConfig::default(), &empty, &empty, &sidecar)
        .expect_err("default config must refuse enable");
    assert!(matches!(refused, EnableError::FeatureDisabled));
    assert_eq!(
        before,
        std::fs::read(&store_path).unwrap(),
        "default build left graph.db untouched"
    );
    assert!(!sidecar.exists());

    // Even when explicitly enabled and persisted, only the sidecar is written:
    // the original SQLite bytes are byte-for-byte identical.
    let store = memnoth_codegraph::Store::open(&store_path).expect("reopen store");
    let existing = snapshot_from_store(&store).expect("snapshot existing store");
    drop(store);
    assert_eq!(existing.nodes.len(), 1);
    let compact = compact_round_trip(&existing).expect("compact round trip");
    assert_eq!(compact, existing, "codec preserves the existing snapshot");

    let enabled = try_enable(&CompactConfig::on(), &existing, &existing, &sidecar)
        .expect("matching fixture enables");
    enabled.persist().expect("write sidecar");

    assert_eq!(
        before,
        std::fs::read(&store_path).unwrap(),
        "graph.db bytes unchanged"
    );
    let written = std::fs::read(&sidecar).expect("read sidecar");
    assert_eq!(&written[..8], graph_compression::FORMAT_MAGIC);
    let mapped = MmapView::from_bytes(written);
    assert_eq!(decode_snapshot(&mapped).unwrap(), existing);
    assert_eq!(encode_snapshot(&existing), mapped.as_slice());
}
