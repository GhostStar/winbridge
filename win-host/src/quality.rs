use std::{
    sync::atomic::{AtomicBool, AtomicU32},
    time::{Duration, Instant},
};

/// Live knobs for one captured window. The capture thread reads them periodically
/// or immediately whenever `generation` is incremented by `SetVideoConfig`.
pub struct RateControl {
    pub fps: AtomicU32,
    pub base_bitrate_kbps: AtomicU32,
    pub rtt_ms: AtomicU32,
    pub adaptive: AtomicBool,
    pub generation: AtomicU32,
}

/// `profile`: 2 = text (at most 30 fps so each frame is sharper),
/// 3 = game (host fps cap), anything else = host defaults.
/// A non-zero request is clamped to the host cap. Text still lowers anything above 30.
pub fn profile_budget(
    profile: i32,
    cap_fps: u32,
    cap_bitrate: u32,
    requested_fps: u32,
    requested_bitrate: u32,
) -> (u32, u32) {
    let cap_fps = cap_fps.max(1);
    let cap_bitrate = cap_bitrate.max(800);
    let mut fps = if requested_fps == 0 {
        cap_fps
    } else {
        requested_fps.min(cap_fps)
    };
    if profile == 2 {
        fps = fps.min(30);
    }
    let bitrate = if requested_bitrate == 0 {
        cap_bitrate
    } else {
        requested_bitrate.min(cap_bitrate)
    };
    (fps.max(1), bitrate.max(800))
}

/// Instantaneous target bitrate when the outbound queue or the client's RTT grows.
/// Never exceeds `base_kbps` and never drops below 800 kbps.
pub fn adapt_bitrate(base_kbps: u32, backlog_frames: usize, rtt_ms: u32) -> u32 {
    let base = base_kbps.max(800);
    let queue_pct: u32 = match backlog_frames {
        0 | 1 => 100,
        2 => 75,
        _ => 50,
    };
    let rtt_pct: u32 = if rtt_ms >= 100 {
        60
    } else if rtt_ms >= 50 {
        80
    } else {
        100
    };
    let scaled = (u64::from(base) * u64::from(queue_pct) * u64::from(rtt_pct) / 10_000) as u32;
    scaled.clamp(800, base)
}

const STEP_DOWN_COOLDOWN: Duration = Duration::from_millis(500);
const STEP_UP_COOLDOWN: Duration = Duration::from_secs(3);
const STEP_DOWN_CONFIRM: u32 = 2;
const STEP_UP_CONFIRM: u32 = 5;

/// Stateful adaptive bitrate controller with hysteresis and cooldowns to prevent
/// rapid bitrate oscillation on transient 1-frame queue spikes or RTT jitter.
pub struct AdaptiveState {
    current_kbps: u32,
    last_change: Instant,
    congested_streak: u32,
    healthy_streak: u32,
}

impl AdaptiveState {
    pub fn new(initial_kbps: u32, now: Instant) -> Self {
        Self {
            current_kbps: initial_kbps.max(800),
            // Allow an immediate reaction once enough samples accumulate after startup.
            last_change: now
                .checked_sub(STEP_UP_COOLDOWN)
                .unwrap_or(now),
            congested_streak: 0,
            healthy_streak: 0,
        }
    }

    /// Evaluates one telemetry tick (expected ~200ms cadence) and returns the smoothed bitrate.
    pub fn sample(
        &mut self,
        now: Instant,
        base_kbps: u32,
        adaptive: bool,
        backlog_frames: usize,
        rtt_ms: u32,
    ) -> u32 {
        let base = base_kbps.max(800);
        if !adaptive {
            self.current_kbps = base;
            self.congested_streak = 0;
            self.healthy_streak = 0;
            self.last_change = now;
            return self.current_kbps;
        }
        if self.current_kbps > base {
            self.current_kbps = base;
            self.last_change = now;
        }
        let target = adapt_bitrate(base, backlog_frames, rtt_ms);
        if target < self.current_kbps {
            self.healthy_streak = 0;
            self.congested_streak = self.congested_streak.saturating_add(1);
            if self.congested_streak >= STEP_DOWN_CONFIRM
                && now.saturating_duration_since(self.last_change) >= STEP_DOWN_COOLDOWN
            {
                self.current_kbps = target;
                self.congested_streak = 0;
                self.last_change = now;
            }
        } else if target > self.current_kbps {
            self.congested_streak = 0;
            self.healthy_streak = self.healthy_streak.saturating_add(1);
            if self.healthy_streak >= STEP_UP_CONFIRM
                && now.saturating_duration_since(self.last_change) >= STEP_UP_COOLDOWN
            {
                // Ramp back up gradually (at most +25% of base per step) to avoid burst stalls.
                let step = (base / 4).max(500);
                self.current_kbps = self.current_kbps.saturating_add(step).min(target);
                self.healthy_streak = 0;
                self.last_change = now;
            }
        } else {
            self.congested_streak = 0;
            self.healthy_streak = 0;
        }
        self.current_kbps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_profile_lowers_fps_but_keeps_bitrate() {
        assert_eq!(profile_budget(2, 60, 15_000, 0, 0), (30, 15_000));
        assert_eq!(profile_budget(2, 60, 15_000, 60, 0), (30, 15_000));
        assert_eq!(profile_budget(3, 60, 15_000, 0, 0), (60, 15_000));
        assert_eq!(profile_budget(1, 60, 15_000, 120, 20_000), (60, 15_000));
    }

    #[test]
    fn adaptation_responds_to_queue_and_rtt_without_exceeding_base() {
        assert_eq!(adapt_bitrate(10_000, 0, 10), 10_000);
        assert_eq!(adapt_bitrate(10_000, 2, 10), 7_500);
        assert_eq!(adapt_bitrate(10_000, 4, 120), 3_000);
        assert_eq!(adapt_bitrate(400, 8, 200), 800);
    }

    #[test]
    fn adaptive_state_dampens_spikes_and_recovers_with_cooldown() {
        let t0 = Instant::now();
        let mut ctrl = AdaptiveState::new(10_000, t0);

        // Single transient queue spike must not drop bitrate.
        assert_eq!(ctrl.sample(t0 + Duration::from_millis(200), 10_000, true, 3, 10), 10_000);
        assert_eq!(ctrl.sample(t0 + Duration::from_millis(400), 10_000, true, 0, 10), 10_000);

        // Two consecutive congested samples step down smoothly.
        assert_eq!(ctrl.sample(t0 + Duration::from_millis(600), 10_000, true, 3, 10), 10_000);
        assert_eq!(ctrl.sample(t0 + Duration::from_millis(800), 10_000, true, 3, 10), 5_000);

        // Immediate healthy samples within the 3s cooldown must NOT oscillate back up.
        for i in 1..=6 {
            let t = t0 + Duration::from_millis(800 + i * 200);
            assert_eq!(ctrl.sample(t, 10_000, true, 0, 10), 5_000);
        }

        // After 3s cooldown with sustained healthy samples, step up by +25% of base (7500).
        let t_recover = t0 + Duration::from_millis(4_000);
        assert_eq!(ctrl.sample(t_recover, 10_000, true, 0, 10), 7_500);

        // Disabling adaptive immediately restores base bitrate.
        assert_eq!(
            ctrl.sample(t_recover + Duration::from_millis(200), 10_000, false, 3, 10),
            10_000
        );
    }
}
