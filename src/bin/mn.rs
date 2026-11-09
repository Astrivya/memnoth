//! `mn` — the short name for the same product.
//!
//! Cargo warns when two `[[bin]]` targets name one `path`, and that warning was
//! the last thing between this tree and a silent build. `include!` keeps the
//! door single-sourced: `src/main.rs` is compiled into this crate root as well,
//! so the stack-size wrapper and the version string cannot drift between the two
//! names. Do not add logic here; change `src/main.rs`.

include!("../main.rs");
