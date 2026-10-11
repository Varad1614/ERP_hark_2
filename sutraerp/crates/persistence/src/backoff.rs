//! Exponential backoff with jitter, shared by the outbox relay and the sweeper scheduler.

use std::time::Duration;

/// `ceiling(n) = min(max, base · 2^(n-1))`; the actual delay uses "equal jitter": a uniformly
/// random value in `[ceiling/2, ceiling]`, so retries of many rows spread out while the delay
/// still grows monotonically on average.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max: max.max(base),
        }
    }

    /// Upper bound of the delay before retry number `attempt` (1-based; 0 is treated as 1).
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let exp = attempt.saturating_sub(1).min(32);
        let factor = 1u64 << exp;
        let base_ms = u64::try_from(self.base.as_millis()).unwrap_or(u64::MAX);
        let max_ms = u64::try_from(self.max.as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(base_ms.saturating_mul(factor).min(max_ms))
    }

    /// Jittered delay given a uniform sample `unit` in `[0, 1)`.
    pub fn delay_with(&self, attempt: u32, unit: f64) -> Duration {
        let c = self.ceiling(attempt);
        let half = c / 2;
        half + (c - half).mul_f64(unit.clamp(0.0, 1.0))
    }

    /// Jittered delay with a fresh random sample.
    pub fn delay(&self, attempt: u32) -> Duration {
        self.delay_with(attempt, random_unit())
    }
}

/// Uniform sample in `[0, 1)` from the OS CSPRNG (falls back to 0.5 if it is unavailable).
pub fn random_unit() -> f64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        return 0.5;
    }
    // 53 random bits → exactly representable in an f64 mantissa.
    (u64::from_le_bytes(b) >> 11) as f64 / (1u64 << 53) as f64
}

/// `d` scaled by a random factor in `[1 - spread, 1 + spread]` (`spread` clamped to `[0, 1]`).
pub fn jittered(d: Duration, spread: f64) -> Duration {
    let s = spread.clamp(0.0, 1.0);
    d.mul_f64(1.0 - s + 2.0 * s * random_unit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_doubles_and_caps() {
        let b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        let secs: Vec<u64> = (0..=8).map(|n| b.ceiling(n).as_secs()).collect();
        assert_eq!(secs, vec![1, 1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(b.ceiling(u32::MAX), Duration::from_secs(60), "no overflow");
    }

    #[test]
    fn jitter_stays_within_half_to_full_ceiling() {
        let b = Backoff::new(Duration::from_millis(500), Duration::from_secs(300));
        for attempt in 1..20 {
            let c = b.ceiling(attempt);
            assert_eq!(b.delay_with(attempt, 0.0), c / 2);
            assert_eq!(b.delay_with(attempt, 1.0), c);
            for _ in 0..50 {
                let d = b.delay(attempt);
                assert!(d >= c / 2 && d <= c, "{d:?} outside [{:?}, {c:?}]", c / 2);
            }
        }
    }

    #[test]
    fn max_below_base_is_raised() {
        let b = Backoff::new(Duration::from_secs(5), Duration::from_secs(1));
        assert_eq!(b.ceiling(3), Duration::from_secs(5));
    }

    #[test]
    fn random_unit_and_spread() {
        for _ in 0..100 {
            let u = random_unit();
            assert!((0.0..1.0).contains(&u));
            let j = jittered(Duration::from_secs(100), 0.1);
            assert!(j >= Duration::from_secs(90) && j <= Duration::from_secs(110));
        }
    }
}
