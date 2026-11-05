//! Spring-driven animation values, frame-rate independent (analytic solver from `springs`).
//! Presets mirror SwiftUI's `.smooth`, `.snappy` and `.bouncy` springs.

use springs::{Spring, SpringConfig};
use std::time::Instant;

#[derive(Clone, Copy)]
pub enum Feel {
    /// Very short, no overshoot: things that track the pointer.
    Quick,
    /// No overshoot; fades and settling.
    Smooth,
    /// Quick with a hint of overshoot; things sliding into place.
    Snappy,
    /// Playful overshoot; small elements popping in.
    Bouncy,
    /// Slow and soft, no overshoot; whole windows appearing.
    Gentle,
    /// Fast, lightly damped oscillation; attention shakes.
    Shake,
}

impl Feel {
    fn config(self) -> SpringConfig {
        let (duration, bounce) = match self {
            Feel::Quick => (0.14, 0.0),
            Feel::Smooth => (0.22, 0.0),
            Feel::Snappy => (0.26, 0.14),
            Feel::Bouncy => (0.36, 0.36),
            Feel::Gentle => (0.42, 0.0),
            Feel::Shake => (0.11, 0.9),
        };
        SpringConfig::from_duration_bounce(duration, bounce)
    }
}

#[derive(Clone, Copy)]
pub struct Motion {
    spring: Spring<f32>,
    last: Instant,
}

impl Motion {
    pub fn new(value: f32, feel: Feel) -> Self {
        Self { spring: Spring::new(value).with_config(feel.config()).with_epsilon(0.002), last: Instant::now() }
    }

    /// Like `new`, with an explicit settle threshold for values not in 0..1 (e.g. pixels).
    pub fn with_epsilon(mut self, epsilon: f64) -> Self {
        self.spring = self.spring.with_epsilon(epsilon);
        self
    }

    pub fn value(&self) -> f32 {
        self.spring.value()
    }

    /// Jumps to `value` with no animation.
    pub fn snap_to(&mut self, value: f32) {
        self.spring.snap_to(value);
    }

    /// Where the spring is heading.
    /// Jumps to the target and stops.
    pub fn settle(&mut self) {
        self.spring.snap_to(self.spring.target());
    }

    pub fn target_value(&self) -> f32 {
        self.spring.target()
    }

    pub fn moving(&self) -> bool {
        !self.spring.is_settled()
    }

    fn wake(&mut self) {
        if !self.moving() {
            // Don't let a long idle period count as elapsed animation time.
            self.last = Instant::now();
        }
    }

    pub fn to(&mut self, target: f32) {
        if target != self.spring.target() {
            self.wake();
            self.spring.set_target(target);
        }
    }

    /// Retargets with a different feel (e.g. bouncy on the way in, smooth on the way out).
    pub fn animate_to(&mut self, target: f32, feel: Feel) {
        self.spring = self.spring.with_config(feel.config());
        self.to(target);
    }

    /// Adds velocity (units per second), e.g. to start a shake around a resting value.
    pub fn kick(&mut self, velocity: f32) {
        self.wake();
        self.spring.add_velocity(velocity);
    }

    /// Advances to `now`. Returns true while still moving.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.moving() {
            self.last = now;
            return false;
        }
        let dt = now.saturating_duration_since(self.last).as_secs_f64().min(0.1);
        self.last = now;
        self.spring.advance(dt);
        self.moving()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn settles_on_target() {
        let start = Instant::now();
        let mut m = Motion::new(0.0, Feel::Snappy);
        m.to(1.0);
        m.last = start;
        let mut t = start;
        let mut peak: f32 = 0.0;
        for _ in 0..120 {
            t += Duration::from_millis(16);
            m.tick(t);
            peak = peak.max(m.value());
        }
        assert!(!m.moving());
        assert_eq!(m.value(), 1.0);
        assert!(peak > 1.0, "snappy should overshoot a little");
    }

    #[test]
    fn shake_returns_to_rest() {
        let start = Instant::now();
        let mut m = Motion::new(0.0, Feel::Shake).with_epsilon(0.05);
        m.kick(800.0);
        m.last = start;
        let mut t = start;
        let mut crossings = 0;
        let mut prev = 0.0f32;
        for _ in 0..200 {
            t += Duration::from_millis(8);
            m.tick(t);
            if prev.signum() != m.value().signum() && prev != 0.0 {
                crossings += 1;
            }
            prev = m.value();
        }
        assert!(crossings >= 4, "shake should oscillate, got {crossings}");
        assert_eq!(m.value(), 0.0);
    }
}
