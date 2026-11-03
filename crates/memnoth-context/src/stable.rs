//! Deterministic hashing for receipts.
//!
//! FNV-1a 64-bit, written out rather than pulled from `std`: `DefaultHasher` is explicitly not
//! stable across Rust releases, and a receipt whose hash changes when the toolchain moves is not
//! a receipt. This is not cryptography — it is an identity for a byte string, used to answer
//! "did the prefix change between these two turns".

const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// Incremental FNV-1a.
#[derive(Clone, Debug)]
pub struct StableHasher {
    state: u64,
}

impl Default for StableHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StableHasher {
    /// Create a new hasher starting at the FNV offset basis.
    pub fn new() -> Self {
        Self {
            state: OFFSET_BASIS,
        }
    }
    /// Hash one string's bytes, then a separator so `("ab", "c")` and `("a", "bc")` differ.
    pub fn write(&mut self, text: &str) {
        for byte in text.as_bytes() {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(PRIME);
        }
        self.state ^= 0xff;
        self.state = self.state.wrapping_mul(PRIME);
    }

    /// Finish hashing and return the 64-bit FNV-1a digest.
    pub fn finish(&self) -> u64 {
        self.state
    }
}

/// Lower-case hex, fixed width, so receipts sort and compare as text.
pub fn hex(value: u64) -> String {
    format!("{value:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_hashes_the_same_way() {
        let mut a = StableHasher::new();
        a.write("hello");
        let mut b = StableHasher::new();
        b.write("hello");
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn a_separator_keeps_concatenations_distinct() {
        let mut a = StableHasher::new();
        a.write("ab");
        a.write("c");
        let mut b = StableHasher::new();
        b.write("a");
        b.write("bc");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn hex_is_fixed_width() {
        assert_eq!(hex(0).len(), 16);
        assert_eq!(hex(u64::MAX), "ffffffffffffffff");
    }

    #[test]
    fn the_empty_string_still_produces_the_offset_basis() {
        let h = StableHasher::new().finish();
        assert_eq!(h, OFFSET_BASIS);
    }
}
