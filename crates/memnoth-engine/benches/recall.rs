//! Recall at 10k beliefs (W12-05): seed an in-memory engine once, then time
//! `search_observations` — the FTS recall path `mn vault recall` uses.
//!
//! Run with `cargo bench -p memnoth-engine --bench recall`. The seeding is
//! outside the timed region (`iter_batched` setup), so the number is the
//! recall query over a 10k-belief vault, not the insert cost.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use memnoth_engine::{EntityKind, MemoryEngine};

const T0: i64 = 1_700_000_000_000;
const BELIEFS: usize = 10_000;

fn seeded() -> MemoryEngine {
    let eng = MemoryEngine::open_in_memory().unwrap();
    for i in 0..BELIEFS {
        let id = eng
            .add_entity(
                EntityKind::Concept,
                &format!("entity{i}"),
                1.0,
                T0 + i as i64,
            )
            .unwrap();
        eng.add_observation(
            id,
            &format!("belief number {i} about entity{i}"),
            "fact",
            1.0,
            T0 + i as i64,
        )
        .unwrap();
    }
    eng
}

fn recall_10k(c: &mut Criterion) {
    let eng = seeded();
    let mut group = c.benchmark_group("recall_10k");
    group.throughput(Throughput::Elements(BELIEFS as u64));
    group.bench_function("search_observations", |b| {
        b.iter_batched(
            || (),
            |()| {
                let rows = eng
                    .search_observations("belief", 10, T0 + BELIEFS as i64)
                    .unwrap();
                std::hint::black_box(rows);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, recall_10k);
criterion_main!(benches);
