//! Budgeted selection: a knapsack, not a truncation.
//!
//! The window is a resource with a price. Filling it by "take everything until it overflows" makes
//! the last thing read the least valuable thing in the prompt, and gives no accounting for what
//! was lost. This module allocates under two ceilings — per slot and overall — and records every
//! drop with a reason, so the receipt can say *what was left out*, not only what went in.
//!
//! Determinism is part of the contract: the same items always produce the same allocation, with
//! no map iteration and no floating point. Density is compared by cross-multiplication
//! (`value_a * tokens_b` vs `value_b * tokens_a`) rather than division, so ties cannot depend on
//! rounding.

use crate::stable::hex;

/// A named region of the prompt with a hard token ceiling.
///
/// Slots exist because a global budget alone lets one cheap-but-large block starve the
/// three lines that actually decide the task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub name: &'static str,
    pub max_tokens: usize,
}

/// The overall ceiling for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Budget {
    pub total: usize,
}

/// A candidate block of context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub slot: &'static str,
    pub text: String,
    pub tokens: usize,
    /// Expected usefulness, in arbitrary units. Only the ratio to `tokens` matters.
    pub value: u32,
}

impl Item {
    /// Build an item, measuring its tokens.
    pub fn new(slot: &'static str, text: impl Into<String>, value: u32) -> Self {
        let text = text.into();
        let tokens = estimate_tokens(&text);
        Self {
            slot,
            text,
            tokens,
            value,
        }
    }

    /// Identity of the block, used to keep receipts comparable across runs.
    pub fn hash(&self) -> String {
        let mut hasher = crate::stable::StableHasher::new();
        hasher.write(self.slot);
        hasher.write(&self.text);
        hex(hasher.finish())
    }
}

/// Why an item did not make it into the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// The slot was already full.
    OverSlot,
    /// The request-level ceiling was reached.
    OverTotal,
    /// The item named a slot the allocator does not know.
    UnknownSlot,
}

/// What survived, in the order it should be rendered, and what did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub kept: Vec<Item>,
    pub dropped: Vec<(Item, DropReason)>,
    /// Tokens actually allocated.
    pub used: usize,
}

/// Rough token count: four characters per token, rounded up, empty is zero.
///
/// Deliberately model-independent and deliberately conservative. A real tokenizer belongs behind
/// a per-model interface; using this number everywhere keeps the budget honest when the tokenizer
/// is unknown, and it is what the receipts record today.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.chars().count().div_ceil(4).max(1)
}

/// Fill slots under both ceilings.
///
/// Order: density (`value / tokens`) descending, then fewer tokens, then slot name, then text.
/// Items naming an unknown slot are dropped rather than silently admitted, because a typo in a
/// slot name is a bug the receipt should show.
pub fn allocate(items: Vec<Item>, slots: &[Slot], budget: &Budget) -> Allocation {
    let mut candidates: Vec<Item> = Vec::with_capacity(items.len());
    let mut dropped: Vec<(Item, DropReason)> = Vec::new();
    // An item naming a slot the allocator does not know is a bug the receipt should show, so it
    // is kept in `dropped` instead of vanishing.
    for item in items {
        if slots.iter().any(|slot| slot.name == item.slot) {
            candidates.push(item);
        } else {
            dropped.push((item, DropReason::UnknownSlot));
        }
    }

    candidates.sort_by(|a, b| {
        let left = u64::from(a.value) * b.tokens.max(1) as u64;
        let right = u64::from(b.value) * a.tokens.max(1) as u64;
        right
            .cmp(&left)
            .then(a.tokens.cmp(&b.tokens))
            .then(a.slot.cmp(b.slot))
            .then(a.text.cmp(&b.text))
    });

    let mut per_slot: Vec<usize> = vec![0; slots.len()];
    let mut used = 0usize;
    let mut kept = Vec::new();

    for item in candidates {
        let index = slots
            .iter()
            .position(|slot| slot.name == item.slot)
            .expect("filtered to known slots");
        let cap = slots[index].max_tokens;
        if per_slot[index] + item.tokens > cap {
            dropped.push((item, DropReason::OverSlot));
            continue;
        }
        if used + item.tokens > budget.total {
            dropped.push((item, DropReason::OverTotal));
            continue;
        }
        per_slot[index] += item.tokens;
        used += item.tokens;
        kept.push(item);
    }

    // Density decided what survived; it must not also decide what the prompt looks like. The
    // layers are rendered in the documented order (identity, policy, decisions, graph, memory,
    // session, tools_hint); a stable sort keeps each layer's own order.
    kept.sort_by_key(|item| {
        slots
            .iter()
            .position(|slot| slot.name == item.slot)
            .unwrap_or(usize::MAX)
    });

    Allocation {
        kept,
        dropped,
        used,
    }
}

/// Alias kept for call sites that read better as "allocate and explain every drop".
pub fn allocate_with(items: Vec<Item>, slots: &[Slot], budget: &Budget) -> Allocation {
    allocate(items, slots, budget)
}
/// The seven named context layers from the MASTER-PLAN §4.2 layer budgets,
/// in render order: identity, policy, decisions, session, memory, graph, tools_hint.
///
/// Memory precedes the graph on purpose: what this workspace remembers about the request is the
/// frame for the code it touches, not a comment on it.
pub fn default_slots() -> Vec<Slot> {
    vec![
        Slot {
            name: "identity",
            max_tokens: 200,
        },
        Slot {
            name: "policy",
            max_tokens: 120,
        },
        Slot {
            name: "decisions",
            max_tokens: 400,
        },
        Slot {
            name: "session",
            max_tokens: 300,
        },
        Slot {
            name: "memory",
            max_tokens: 300,
        },
        Slot {
            name: "graph",
            max_tokens: 700,
        },
        Slot {
            name: "tools_hint",
            max_tokens: 120,
        },
    ]
}

/// The overall ceiling for one request: the sum of the seven default slot
/// ceilings (identity 200 + policy 120 + decisions 400 + graph 700 + memory
/// 300 + session 300 + tools_hint 120 = 2,140 tokens).
pub fn default_budget(total: usize) -> Budget {
    Budget { total }
}

/// Convenience constructor: the seven default layers plus a budget whose total
/// is the sum of their per-slot ceilings.
pub fn slot_budget(total: usize) -> (Vec<Slot>, Budget) {
    (default_slots(), default_budget(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots() -> Vec<Slot> {
        vec![
            Slot {
                name: "rules",
                max_tokens: 10,
            },
            Slot {
                name: "files",
                max_tokens: 10,
            },
        ]
    }

    #[test]
    fn tokens_round_up_and_empty_is_free() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn a_slot_ceiling_is_enforced_even_when_the_global_budget_is_large() {
        let items = vec![
            Item::new("rules", "a".repeat(40), 10), // 10 tokens
            Item::new("rules", "b", 1),             // 1 token, must not fit
        ];
        let allocation = allocate(items, &slots(), &Budget { total: 1_000 });
        assert_eq!(allocation.kept.len(), 1);
        assert_eq!(allocation.dropped[0].1, DropReason::OverSlot);
    }

    #[test]
    fn the_global_budget_stops_allocation_across_slots() {
        let items = vec![
            Item::new("rules", "a".repeat(24), 10), // 6 tokens
            Item::new("files", "b".repeat(24), 10), // 6 tokens
        ];
        let allocation = allocate(items, &slots(), &Budget { total: 8 });
        assert_eq!(allocation.kept.len(), 1);
        assert_eq!(allocation.used, 6);
        assert!(matches!(
            allocation.dropped[0].1,
            DropReason::OverSlot | DropReason::OverTotal
        ));
    }

    #[test]
    fn density_wins_over_raw_value() {
        let items = vec![
            Item::new("rules", "a".repeat(400), 100), // 100 tokens, value 100 → density 1
            Item::new("files", "b".repeat(40), 50),   // 10 tokens, value 50 → density 5
        ];
        let allocation = allocate(items, &slots(), &Budget { total: 10 });
        assert_eq!(allocation.kept.len(), 1);
        assert_eq!(allocation.kept[0].text, "b".repeat(40));
    }

    #[test]
    fn allocation_is_deterministic_for_equal_density() {
        let build = || {
            vec![
                Item::new("rules", "same", 1),
                Item::new("files", "same", 1),
                Item::new("rules", "other", 1),
            ]
        };
        let a = allocate(build(), &slots(), &Budget { total: 100 });
        let b = allocate(build(), &slots(), &Budget { total: 100 });
        assert_eq!(a, b);
    }

    #[test]
    fn an_unknown_slot_is_reported_not_silently_admitted() {
        let allocation = allocate_with(
            vec![Item::new("typo", "x", 1)],
            &slots(),
            &Budget { total: 100 },
        );
        assert!(allocation.kept.is_empty());
        assert_eq!(allocation.dropped[0].1, DropReason::UnknownSlot);
    }

    #[test]
    fn every_dropped_item_carries_a_reason() {
        let items = vec![
            Item::new("rules", "a".repeat(400), 1),
            Item::new("files", "b".repeat(400), 1),
        ];
        let allocation = allocate(items, &slots(), &Budget { total: 10 });
        assert!(!allocation.dropped.is_empty());
        for (_, reason) in &allocation.dropped {
            assert!(matches!(
                reason,
                DropReason::OverSlot | DropReason::OverTotal | DropReason::UnknownSlot
            ));
        }
    }

    #[test]
    fn default_slots_has_seven_named_layers() {
        let slots = default_slots();
        assert_eq!(slots.len(), 7);
        let names: Vec<&str> = slots.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "identity",
                "policy",
                "decisions",
                "session",
                "memory",
                "graph",
                "tools_hint"
            ]
        );
        let expected: Vec<(&str, usize)> = vec![
            ("identity", 200),
            ("policy", 120),
            ("decisions", 400),
            ("session", 300),
            ("memory", 300),
            ("graph", 700),
            ("tools_hint", 120),
        ];
        for (slot, (name, max_tokens)) in slots.iter().zip(expected.iter()) {
            assert_eq!(slot.name, *name);
            assert_eq!(slot.max_tokens, *max_tokens);
        }
    }

    #[test]
    fn default_slots_total_equals_sum_of_per_slot_ceilings() {
        let slots = default_slots();
        let total: usize = slots.iter().map(|s| s.max_tokens).sum();
        assert_eq!(total, 2140);
        let (slots2, budget) = slot_budget(total);
        assert_eq!(slots2.len(), slots.len());
        assert_eq!(budget.total, total);
    }

    #[test]
    fn default_budget_constructs_total_ceiling() {
        let budget = default_budget(42);
        assert_eq!(budget.total, 42);
    }

    #[test]
    fn allocate_respects_each_default_slot_ceiling() {
        let slots = default_slots();
        // identity: 200 token ceiling. Three items of 100 tokens each → the last is refused.
        let items = vec![
            Item::new("identity", "a".repeat(400), 10), // 100 tokens
            Item::new("identity", "b".repeat(400), 10), // 100 tokens
            Item::new("identity", "c".repeat(400), 10), // 100 tokens, over slot ceiling alone
        ];
        let allocation = allocate(items, &slots, &default_budget(1000));
        assert_eq!(allocation.kept.len(), 2);
        assert_eq!(allocation.dropped.len(), 1);
        assert_eq!(allocation.dropped[0].1, DropReason::OverSlot);
    }

    #[test]
    fn unknown_slot_is_dropped_with_default_slots() {
        let slots = default_slots();
        let allocation = allocate(
            vec![Item::new("nope", "x", 1)],
            &slots,
            &default_budget(100),
        );
        assert!(allocation.kept.is_empty());
        assert_eq!(allocation.dropped[0].1, DropReason::UnknownSlot);
    }
}
