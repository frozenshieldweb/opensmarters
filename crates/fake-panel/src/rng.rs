//! Tiny deterministic PRNG and hashing helpers.
//!
//! The catalogue, the EPG and the quirk decisions must be identical from run to run for
//! a given seed, so nothing here touches OS randomness.

/// SplitMix64 finaliser: a good 64-bit mixer.
pub(crate) fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a over a string, then mixed.
pub(crate) fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    splitmix(h)
}

/// Combine two values into one well-mixed hash.
pub(crate) fn mix(a: u64, b: u64) -> u64 {
    splitmix(a ^ splitmix(b).rotate_left(17))
}

/// Combine three values.
pub(crate) fn mix3(a: u64, b: u64, c: u64) -> u64 {
    mix(mix(a, b), c)
}

/// A small sequential generator (SplitMix64 stream).
#[derive(Debug, Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(splitmix(seed))
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        splitmix(self.0)
    }

    /// Uniform in `0..n` (`n` must be > 0; returns 0 otherwise).
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// Uniform in `lo..=hi`.
    pub(crate) fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo) + 1)
    }

    /// `true` with probability `percent`/100.
    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// Pick an element of a non-empty slice.
    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let idx = self.below(items.len() as u64) as usize;
        &items[idx.min(items.len().saturating_sub(1))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_eq!(hash_str("abc"), hash_str("abc"));
        assert_ne!(hash_str("abc"), hash_str("abd"));
    }

    #[test]
    fn range_bounds() {
        let mut r = Rng::new(1);
        for _ in 0..1000 {
            let v = r.range(3, 7);
            assert!((3..=7).contains(&v));
        }
    }
}
