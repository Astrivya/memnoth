//! Budget tracking across child runs.
//!
//! A [`BudgetLedger`] holds a total token/USD allowance and decrements it as
//! children [`reserve`](BudgetLedger::reserve) ahead of making requests. A
//! reservation that would exceed what remains is refused *before* the request
//! is made. Children are narrowed via [`Permission::glb`] so a child can
//! never exceed its parent's authority.

use crate::error::{AgentError, ErrorCode};
use crate::spec::{Budget, Permission};

/// A ledger of remaining token/USD budget across child runs.
#[derive(Debug, Clone)]
pub struct BudgetLedger {
    /// Total allowance; `None` in a field means unbounded in that dimension.
    total: Budget,
    /// Tokens already reserved.
    reserved_tokens: u64,
    /// USD already reserved.
    reserved_usd: f64,
}

impl BudgetLedger {
    /// Create a ledger with `total` as the allowance.
    pub fn new(total: Budget) -> Self {
        Self {
            total,
            reserved_tokens: 0,
            reserved_usd: 0.0,
        }
    }

    /// Return the remaining allowance as a [`Budget`].
    ///
    /// A field is `None` when the total was unset (unbounded in that
    /// dimension).
    pub fn remaining(&self) -> Budget {
        Budget {
            tokens: self
                .total
                .tokens
                .map(|t| t.saturating_sub(self.reserved_tokens)),
            usd: self.total.usd.map(|u| (u - self.reserved_usd).max(0.0)),
        }
    }

    /// Reserve `tokens` and `usd` for a child run.
    ///
    /// Refuses with [`ErrorCode::Budget`] when the request would exceed the
    /// remaining allowance, *before* any request is made. On success the
    /// reservation is deducted from what remains.
    pub fn reserve(&mut self, tokens: u64, usd: f64) -> Result<(), AgentError> {
        let remaining = self.remaining();

        if let Some(limit) = remaining.tokens
            && tokens > limit
        {
            return Err(AgentError::new(
                ErrorCode::Budget,
                format!("child requests {tokens} tokens but only {limit} remain"),
            ));
        }

        if let Some(limit) = remaining.usd
            && usd > limit
        {
            return Err(AgentError::new(
                ErrorCode::Budget,
                format!("child requests ${usd} but only ${limit:.4} remain"),
            ));
        }

        self.reserved_tokens = self.reserved_tokens.saturating_add(tokens);
        self.reserved_usd = (self.reserved_usd + usd).max(0.0);
        Ok(())
    }

    /// Narrow `child` by `parent` via [`Permission::glb`].
    ///
    /// Returns the narrowed permission (the child is narrowed, never widened)
    /// plus one sentence per narrowing made.
    pub fn narrow(parent: &Permission, child: &Permission) -> (Permission, Vec<String>) {
        child.glb(parent)
    }
}
