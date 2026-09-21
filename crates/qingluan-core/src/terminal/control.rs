//! Control-lease generation.

use std::num::NonZeroU64;

/// Generation of one session's control lease.
///
/// Each takeover — a fresh acquisition, or re-acquisition after expiry or
/// release — mints a new generation, and a write accepted for generation
/// `N` must not commit once the current generation is higher: control is
/// isolated by generation, so a late token can never submit new write
/// fragments. Zero is not a generation (it is the reserved "no lease"
/// value), so an uninitialized/default value can never match a real one;
/// the first generation is 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControlGeneration(NonZeroU64);

impl ControlGeneration {
    /// Construct a generation from a raw value; zero is rejected.
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    /// The first generation of a session's control sequence.
    pub fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    /// The next generation, or `None` when the sequence is exhausted.
    pub fn successor(self) -> Option<Self> {
        self.0.get().checked_add(1).and_then(Self::new)
    }

    /// Numeric generation value (always non-zero).
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_not_a_generation() {
        assert_eq!(ControlGeneration::new(0), None);
        assert_eq!(ControlGeneration::new(1).expect("1 is valid").get(), 1);
        assert_eq!(
            ControlGeneration::new(u64::MAX)
                .expect("max is valid")
                .get(),
            u64::MAX
        );
    }

    #[test]
    fn generations_advance_strictly_and_are_ordered() {
        let first = ControlGeneration::first();
        assert_eq!(first.get(), 1);
        let second = first.successor().expect("1 has a successor");
        assert_eq!(second.get(), 2);
        assert!(second > first, "a new generation must outrank the old one");
        assert_eq!(second, ControlGeneration::new(2).expect("2 is valid"));

        // Exhaustion is explicit, never a wrap onto a live generation.
        let last = ControlGeneration::new(u64::MAX).expect("max is valid");
        assert_eq!(last.successor(), None);
    }
}
