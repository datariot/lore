//! Access counter used by heading nodes.
//!
//! Each `HeadingNode` carries one of these. `get_section` bumps it on every
//! read, `recent_hot` and the BM25 boost read aggregate counts. The counter
//! lives only in memory — we never serialize it, so a freshly-loaded corpus
//! starts from zero.
//!
//! The newtype lets `HeadingNode` derive `Clone` and `PartialEq` cleanly:
//!
//! - `Clone` copies the current value.
//! - `PartialEq` always returns `true`: two nodes with the same structure
//!   are equal regardless of usage. Round-trip tests can't distinguish a
//!   freshly-loaded (zero) counter from a live one otherwise.

use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Default)]
pub struct AccessCounter(AtomicU32);

impl AccessCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn bump(&self) -> u32 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

impl Clone for AccessCounter {
    fn clone(&self) -> Self {
        Self(AtomicU32::new(self.get()))
    }
}

impl PartialEq for AccessCounter {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for AccessCounter {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_and_read() {
        let c = AccessCounter::new();
        assert_eq!(c.get(), 0);
        c.bump();
        c.bump();
        assert_eq!(c.get(), 2);
    }

    #[test]
    fn clone_copies_current_value() {
        let c = AccessCounter::new();
        c.bump();
        let c2 = c.clone();
        assert_eq!(c2.get(), 1);
        c.bump();
        assert_eq!(c.get(), 2);
        assert_eq!(c2.get(), 1);
    }

    #[test]
    fn equality_ignores_count() {
        let a = AccessCounter::new();
        let b = AccessCounter::new();
        b.bump();
        assert_eq!(a, b);
    }
}
