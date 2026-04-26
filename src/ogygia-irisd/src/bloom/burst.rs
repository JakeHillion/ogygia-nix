//! Burst detection for deletion events using a CUSUM algorithm.
//!
//! Detects bursts of deletion activity to suppress bloom-filter rebuilds
//! during high-churn periods.  When a burst is active, `should_rebuild`
//! returns `false` so that the filter is not rebuilt while the deletion
//! rate is elevated.  After the burst subsides the detector enters a
//! cooldown grace period before returning to `Steady`.

use std::time::Duration;
use std::time::Instant;

/// Operating regime of the burst detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// Normal steady-state operation.
    Steady,
    /// Deletion burst detected; rebuilds are suppressed.
    Burst,
    /// Cooldown period after a burst before returning to Steady.
    Cooldown,
}

/// Detects bursts of deletion events using a CUSUM algorithm.
pub struct BurstDetector {
    k: f64,
    h: f64,
    max_cooldown: Duration,
    cusum: f64,
    regime: Regime,
    burst_start: Option<Instant>,
    cooldown_start: Option<Instant>,
    last_tick: Instant,
}

impl BurstDetector {
    /// Create a new burst detector.
    ///
    /// # Arguments
    /// * `k` — CUSUM reference value (sensitivity).
    /// * `h` — Threshold for entering the `Burst` regime.  `0.0` disables
    ///   burst detection entirely.
    /// * `max_cooldown` — Maximum time to stay in `Burst` before a rebuild
    ///   is forced, and also the cooldown duration before returning to
    ///   `Steady`.
    pub fn new(k: f64, h: f64, max_cooldown: Duration) -> Self {
        Self {
            k,
            h,
            max_cooldown,
            cusum: 0.0,
            regime: Regime::Steady,
            burst_start: None,
            cooldown_start: None,
            last_tick: Instant::now(),
        }
    }

    /// Record a deletion event.
    pub fn observe_deletion(&mut self) {
        self.cusum = (self.cusum + 1.0 - self.k).max(0.0);
        if self.h > 0.0 && self.cusum > self.h && self.regime != Regime::Burst {
            self.regime = Regime::Burst;
            self.burst_start = Some(Instant::now());
        }
    }

    /// Advance the detector's internal clock to `now`, applying CUSUM
    /// decay and evaluating regime transitions.
    pub fn tick(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last_tick);
        self.last_tick = now;
        self.cusum = (self.cusum - elapsed.as_secs_f64()).max(0.0);

        match self.regime {
            Regime::Burst => {
                if self.cusum == 0.0 {
                    if let Some(start) = self.burst_start {
                        if now > start + self.max_cooldown {
                            self.regime = Regime::Steady;
                            self.burst_start = None;
                        } else {
                            self.regime = Regime::Cooldown;
                            self.cooldown_start = Some(now);
                        }
                    } else {
                        self.regime = Regime::Cooldown;
                        self.cooldown_start = Some(now);
                    }
                }
            }
            Regime::Cooldown => {
                if self.cusum > self.h {
                    self.regime = Regime::Burst;
                    self.burst_start = Some(now);
                } else if let Some(start) = self.cooldown_start
                    && now > start + self.max_cooldown
                {
                    self.regime = Regime::Steady;
                }
            }
            Regime::Steady => {}
        }
    }

    /// Current operating regime.
    #[allow(dead_code)]
    pub fn regime(&self) -> Regime {
        self.regime
    }

    /// Current CUSUM value.
    #[allow(dead_code)]
    pub fn cusum(&self) -> f64 {
        self.cusum
    }

    /// Whether a rebuild should proceed given the current deletion ratio
    /// and time.
    ///
    /// Returns `false` during `Burst` or `Cooldown` to suppress rebuilds.
    /// Returns `true` in `Steady` when `deletion_ratio` exceeds the
    /// threshold (0.005).  Forces `true` if the detector has been in
    /// `Burst` for longer than `max_cooldown`.
    pub fn should_rebuild(&self, deletion_ratio: f64, now: Instant) -> bool {
        const REBUILD_THRESHOLD: f64 = 0.005;
        if self.h == 0.0 {
            return deletion_ratio > REBUILD_THRESHOLD;
        }
        match self.regime {
            Regime::Burst => {
                if let Some(start) = self.burst_start
                    && now > start + self.max_cooldown
                {
                    return true;
                }
                false
            }
            Regime::Cooldown => false,
            Regime::Steady => deletion_ratio > REBUILD_THRESHOLD,
        }
    }

    /// Mark that a rebuild has occurred, resetting CUSUM and regime.
    pub fn mark_rebuilt(&mut self, _now: Instant) {
        self.cusum = 0.0;
        self.regime = Regime::Steady;
        self.burst_start = None;
        self.cooldown_start = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_steady_no_burst() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        for _ in 0..2 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_steady_to_burst() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
    }

    #[test]
    fn test_burst_to_cooldown() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        detector.tick(now + Duration::from_secs(5));
        assert_eq!(detector.regime(), Regime::Cooldown);
    }

    #[test]
    fn test_cooldown_to_steady() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(1));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        detector.tick(now + Duration::from_secs(5));
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_burst_resumed_from_cooldown() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        detector.tick(now + Duration::from_secs(5));
        assert_eq!(detector.regime(), Regime::Cooldown);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
    }

    #[test]
    fn test_should_rebuild_steady_below_threshold() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        assert!(!detector.should_rebuild(0.001, now));
    }

    #[test]
    fn test_should_rebuild_burst_suppresses() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert!(!detector.should_rebuild(0.01, now));
    }

    #[test]
    fn test_should_rebuild_cooldown_suppresses() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        detector.tick(now + Duration::from_secs(5));
        assert_eq!(detector.regime(), Regime::Cooldown);
        assert!(!detector.should_rebuild(0.01, now + Duration::from_secs(5)));
    }

    #[test]
    fn test_should_rebuild_steady_above_threshold() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        assert!(detector.should_rebuild(0.01, now));
    }

    #[test]
    fn test_forced_rebuild_after_max_cooldown() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_millis(100));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        assert!(detector.should_rebuild(0.01, now + Duration::from_millis(150)));
    }

    #[test]
    fn test_cusum_reset_on_rebuild() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        assert!(detector.cusum() > 0.0);
        detector.mark_rebuilt(now);
        assert_eq!(detector.cusum(), 0.0);
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_disabled_burst_detection() {
        let mut detector = BurstDetector::new(0.5, 0.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..100 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_rapid_deletions_trigger_burst() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(1));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..200 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        assert!(!detector.should_rebuild(0.01, now));
        detector.tick(now + Duration::from_secs(200));
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_slow_trickle_does_not_trigger_burst() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for i in 0..200 {
            detector.observe_deletion();
            detector.tick(now + Duration::from_secs((i + 1) * 10));
        }
        assert_eq!(detector.regime(), Regime::Steady);
        assert!(detector.should_rebuild(0.01, now + Duration::from_secs(200 * 10)));
    }

    #[test]
    fn test_intermittent_burst_resets_cooldown() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        detector.tick(now + Duration::from_secs(5));
        assert_eq!(detector.regime(), Regime::Cooldown);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
    }

    #[test]
    fn test_forced_rebuild_timing() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_millis(100));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        let later = now + Duration::from_millis(150);
        assert!(detector.should_rebuild(0.01, later));
        detector.mark_rebuilt(later);
        assert_eq!(detector.cusum(), 0.0);
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_cusum_accumulation_pattern() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..5 {
            detector.observe_deletion();
        }
        assert_eq!(detector.cusum(), 2.5);
        detector.tick(now);
        assert_eq!(detector.cusum(), 2.5);
        for _ in 0..2 {
            detector.observe_deletion();
        }
        assert_eq!(detector.cusum(), 3.5);
        assert_eq!(detector.regime(), Regime::Burst);
    }

    #[test]
    fn test_zero_h_disables_burst_detection() {
        let mut detector = BurstDetector::new(0.5, 0.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..100 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Steady);
        assert!(!detector.should_rebuild(0.001, now));
        assert!(detector.should_rebuild(0.01, now));
    }

    #[test]
    fn test_mark_rebuilt_resets_everything() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        assert!(detector.cusum() > 0.0);
        detector.mark_rebuilt(now);
        assert_eq!(detector.cusum(), 0.0);
        assert_eq!(detector.regime(), Regime::Steady);
    }

    #[test]
    fn test_cooldown_expires_allows_rebuild() {
        let mut detector = BurstDetector::new(0.5, 3.0, Duration::from_secs(5));
        let now = Instant::now();
        detector.tick(now);
        for _ in 0..7 {
            detector.observe_deletion();
        }
        assert_eq!(detector.regime(), Regime::Burst);
        let cooldown_time = now + Duration::from_secs(4);
        detector.tick(cooldown_time);
        assert_eq!(detector.regime(), Regime::Cooldown);
        assert!(!detector.should_rebuild(0.01, cooldown_time));
        let steady_time = now + Duration::from_secs(10);
        detector.tick(steady_time);
        assert_eq!(detector.regime(), Regime::Steady);
        assert!(detector.should_rebuild(0.01, steady_time));
    }
}
