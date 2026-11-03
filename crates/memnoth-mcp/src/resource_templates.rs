//! The resource templates this server advertises.
//!
//! Definitions, not data: they are served before any workspace is open, because a host that
//! cannot discover a template cannot ask for one. The reader for each template lives with the
//! subsystem it belongs to (`symbol_resource` for `kg://`), so a template and its answer are
//! never more than one file apart.

use serde_json::{Value, json};

use crate::symbol_resource;

pub(crate) fn all() -> Vec<Value> {
    vec![
        json!({
            "uriTemplate": "mem://entity/{id}",
            "name": "Entity page",
            "description": "One entity with its current observations and outgoing relations; id is the numeric entity id",
            "mimeType": "application/json",
        }),
        json!({
            "uriTemplate": "mem://related/{id}?hops=n",
            "name": "Related graph walk",
            "description": "Typed-relation edges within n hops of an entity (hops optional, default 1, max 3)",
            "mimeType": "application/json",
        }),
        symbol_resource::template(),
    ]
}
