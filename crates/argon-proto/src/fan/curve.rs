// SPDX-License-Identifier: GPL-3.0-or-later
//! Temperature-to-duty mapping, and the hysteresis that stops it oscillating.

use super::{FanDuty, MIN_SPINNING_DUTY};
use alloc::vec::Vec;
use core::fmt;

/// One point on a fan curve: at or above `decicelsius`, run at `duty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePoint {
    /// Temperature threshold in tenths of a degree Celsius.
    ///
    /// Tenths rather than a float so points compare and deduplicate exactly. A curve is
    /// configuration, and configuration that silently collapses two nearly-equal thresholds
    /// is worse than configuration that rejects them.
    pub decicelsius: i32,
    /// Duty to run at from this threshold up to the next.
    pub duty: FanDuty,
}

impl CurvePoint {
    /// Builds a point from whole degrees.
    #[must_use]
    pub const fn from_celsius(temp_c: i32, duty: FanDuty) -> Self {
        Self {
            decicelsius: temp_c * 10,
            duty,
        }
    }

    /// The threshold in whole degrees, truncated.
    #[must_use]
    pub const fn celsius(&self) -> i32 {
        self.decicelsius / 10
    }
}

/// A validated fan curve.
///
/// Points are kept sorted ascending by temperature. Upstream sorts *descending* by
/// formatting each entry into a fixed-width string and sorting the strings, which works only
/// while every temperature has the same number of digits and misorders silently when one
/// does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FanCurve {
    points: Vec<CurvePoint>,
}

impl FanCurve {
    /// Validates and builds a curve.
    ///
    /// # Errors
    ///
    /// Rejects an empty curve, duplicate thresholds, and a curve whose duty falls as
    /// temperature rises. None is silently repaired: a fan curve that does something other
    /// than what the file says is a safety problem, not a convenience.
    pub fn new(mut points: Vec<CurvePoint>) -> Result<Self, CurveError> {
        if points.is_empty() {
            return Err(CurveError::Empty);
        }
        points.sort_by_key(|p| p.decicelsius);

        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.decicelsius == b.decicelsius {
                return Err(CurveError::DuplicateThreshold(a.celsius()));
            }
            if b.duty.percent() < a.duty.percent() {
                return Err(CurveError::NotMonotonic {
                    lower_temp: a.celsius(),
                    lower_duty: a.duty.percent(),
                    higher_temp: b.celsius(),
                    higher_duty: b.duty.percent(),
                });
            }
        }
        Ok(Self { points })
    }

    /// The curve's points, ascending by temperature.
    #[must_use]
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// The duty for a temperature, ignoring hysteresis.
    ///
    /// Below the lowest threshold the fan is off. At or above a threshold, that threshold's
    /// duty applies until the next one.
    #[must_use]
    pub fn duty_for(&self, decicelsius: i32) -> FanDuty {
        let mut duty = FanDuty::Off;
        for p in &self.points {
            if decicelsius >= p.decicelsius {
                duty = p.duty;
            } else {
                break;
            }
        }
        duty
    }

    /// The curve's lowest threshold, in tenths of a degree.
    #[must_use]
    pub fn lowest_threshold(&self) -> Option<i32> {
        self.points.first().map(|p| p.decicelsius)
    }
}

/// Why a fan curve was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveError {
    /// The curve had no points.
    Empty,
    /// Two points shared a threshold.
    DuplicateThreshold(i32),
    /// Duty fell as temperature rose.
    NotMonotonic {
        /// The lower temperature.
        lower_temp: i32,
        /// Its duty.
        lower_duty: u8,
        /// The higher temperature.
        higher_temp: i32,
        /// Its lower duty.
        higher_duty: u8,
    },
}

impl fmt::Display for CurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a fan curve needs at least one point"),
            Self::DuplicateThreshold(t) => write!(f, "two curve points both set at {t}C"),
            Self::NotMonotonic {
                lower_temp,
                lower_duty,
                higher_temp,
                higher_duty,
            } => write!(
                f,
                "curve goes down as it heats up: {lower_temp}C={lower_duty}% but \
                 {higher_temp}C={higher_duty}%"
            ),
        }
    }
}

impl core::error::Error for CurveError {}

/// Applies a fan curve with hysteresis, so a temperature sitting on a threshold does not make
/// the fan oscillate.
///
/// Hysteresis is on **temperature**, not time. Upstream delays every speed *decrease* by
/// thirty seconds, which slows oscillation without preventing it — a temperature hovering on
/// a boundary still flips, just more slowly. Requiring the temperature to fall a margin below
/// the threshold before stepping down removes the oscillation itself.
#[derive(Debug, Clone)]
pub struct FanController {
    curve: FanCurve,
    hysteresis_dc: i32,
    min_duty: u8,
    allow_stop: bool,
    current: Option<FanDuty>,
}

impl FanController {
    /// Creates a controller.
    ///
    /// `hysteresis_c` is in whole degrees. `min_duty` raises any non-zero result below it.
    /// `allow_stop` must be set for the controller ever to return [`FanDuty::Off`] —
    /// stopping a fan is a decision, not an arithmetic outcome.
    #[must_use]
    pub const fn new(curve: FanCurve, hysteresis_c: i32, min_duty: u8, allow_stop: bool) -> Self {
        Self {
            curve,
            hysteresis_dc: hysteresis_c * 10,
            min_duty,
            allow_stop,
            current: None,
        }
    }

    /// The duty currently applied, if the controller has run at least once.
    #[must_use]
    pub const fn current(&self) -> Option<FanDuty> {
        self.current
    }

    /// The curve in use.
    #[must_use]
    pub const fn curve(&self) -> &FanCurve {
        &self.curve
    }

    /// Computes the duty for a temperature, given what is currently applied.
    ///
    /// Increases take effect immediately. Decreases wait until the temperature has fallen
    /// `hysteresis_c` below the threshold that would justify them.
    pub fn update(&mut self, decicelsius: i32) -> FanDuty {
        let target = self.curve.duty_for(decicelsius);

        let chosen = match self.current {
            // First reading, or the fan should speed up: act at once.
            None => target,
            Some(current) if target.percent() >= current.percent() => target,
            // Stepping down: only once the temperature has cleared the margin.
            Some(current) => {
                let cooled = self.curve.duty_for(decicelsius + self.hysteresis_dc);
                if cooled.percent() < current.percent() {
                    target
                } else {
                    current
                }
            }
        };

        let applied = self.apply_floor(chosen);
        self.current = Some(applied);
        applied
    }

    /// Forgets the applied duty, so the next update takes effect without hysteresis.
    pub fn reset(&mut self) {
        self.current = None;
    }

    /// Raises a non-zero duty to the floor, and refuses to stop unless allowed.
    fn apply_floor(&self, duty: FanDuty) -> FanDuty {
        match duty {
            FanDuty::Off if self.allow_stop => FanDuty::Off,
            // Refusing to stop must not mean refusing to cool: fall back to the floor, and
            // never below the duty at which the fan physically turns.
            FanDuty::Off => FanDuty::Percent(self.min_duty.clamp(MIN_SPINNING_DUTY, 100)),
            FanDuty::Percent(p) => FanDuty::Percent(p.max(self.min_duty).min(100)),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{vec, vec::Vec};

    /// The curve the vendor's installer writes, and the one on the development machine.
    fn stock() -> FanCurve {
        FanCurve::new(vec![
            CurvePoint::from_celsius(55, FanDuty::Percent(30)),
            CurvePoint::from_celsius(60, FanDuty::Percent(55)),
            CurvePoint::from_celsius(65, FanDuty::Percent(100)),
        ])
        .unwrap()
    }

    #[test]
    fn below_the_lowest_threshold_the_fan_is_off() {
        assert_eq!(stock().duty_for(400), FanDuty::Off);
        assert_eq!(stock().duty_for(549), FanDuty::Off);
    }

    #[test]
    fn a_threshold_is_inclusive() {
        assert_eq!(stock().duty_for(550), FanDuty::Percent(30));
        assert_eq!(stock().duty_for(600), FanDuty::Percent(55));
        assert_eq!(stock().duty_for(650), FanDuty::Percent(100));
    }

    #[test]
    fn above_the_top_the_top_duty_holds() {
        assert_eq!(stock().duty_for(900), FanDuty::Percent(100));
        assert_eq!(stock().duty_for(i32::MAX), FanDuty::Percent(100));
    }

    #[test]
    fn duty_never_falls_as_temperature_rises() {
        let curve = stock();
        let mut last = 0u8;
        for dc in -500..1000 {
            let d = curve.duty_for(dc).percent();
            assert!(
                d >= last,
                "duty dropped from {last} to {d} at {dc} decicelsius"
            );
            last = d;
        }
    }

    #[test]
    fn input_order_does_not_matter() {
        let forwards = stock();
        let backwards = FanCurve::new(vec![
            CurvePoint::from_celsius(65, FanDuty::Percent(100)),
            CurvePoint::from_celsius(55, FanDuty::Percent(30)),
            CurvePoint::from_celsius(60, FanDuty::Percent(55)),
        ])
        .unwrap();
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn two_and_three_digit_thresholds_sort_numerically() {
        // The failure this guards against: upstream sorts curve entries by formatting them
        // into fixed-width strings and sorting the strings. That is correct only while every
        // temperature has the same number of digits.
        let curve = FanCurve::new(vec![
            CurvePoint::from_celsius(100, FanDuty::Percent(100)),
            CurvePoint::from_celsius(9, FanDuty::Percent(20)),
            CurvePoint::from_celsius(60, FanDuty::Percent(55)),
        ])
        .unwrap();
        let temps: Vec<i32> = curve.points().iter().map(CurvePoint::celsius).collect();
        assert_eq!(
            temps,
            vec![9, 60, 100],
            "curve points are not in numeric order"
        );
        assert_eq!(curve.duty_for(950), FanDuty::Percent(55));
        assert_eq!(curve.duty_for(1000), FanDuty::Percent(100));
    }

    #[test]
    fn a_configured_low_duty_is_honoured_not_silently_raised() {
        // Upstream rewrites any configured duty below 25 to 25, so a documented `55=10`
        // curve point actually produces 25%. The curve layer reports what was configured;
        // raising to a floor is the controller's job and is explicit there.
        let curve =
            FanCurve::new(vec![CurvePoint::from_celsius(55, FanDuty::Percent(10))]).unwrap();
        assert_eq!(curve.duty_for(600), FanDuty::Percent(10));
    }

    #[test]
    fn invalid_curves_are_rejected_rather_than_repaired() {
        assert_eq!(FanCurve::new(Vec::new()), Err(CurveError::Empty));

        assert_eq!(
            FanCurve::new(vec![
                CurvePoint::from_celsius(55, FanDuty::Percent(30)),
                CurvePoint::from_celsius(55, FanDuty::Percent(40)),
            ]),
            Err(CurveError::DuplicateThreshold(55))
        );

        assert!(matches!(
            FanCurve::new(vec![
                CurvePoint::from_celsius(55, FanDuty::Percent(80)),
                CurvePoint::from_celsius(65, FanDuty::Percent(40)),
            ]),
            Err(CurveError::NotMonotonic { .. })
        ));
    }

    #[test]
    fn speeding_up_is_immediate() {
        let mut c = FanController::new(stock(), 3, 10, true);
        assert_eq!(c.update(500), FanDuty::Off);
        assert_eq!(
            c.update(650),
            FanDuty::Percent(100),
            "an increase must not be delayed"
        );
    }

    #[test]
    fn slowing_down_waits_for_the_hysteresis_margin() {
        let mut c = FanController::new(stock(), 3, 10, true);
        assert_eq!(c.update(650), FanDuty::Percent(100));
        // Just below the 65C threshold: still within the margin, so hold.
        assert_eq!(c.update(649), FanDuty::Percent(100));
        assert_eq!(c.update(630), FanDuty::Percent(100));
        // Now clear of it.
        assert_eq!(c.update(619), FanDuty::Percent(55));
    }

    #[test]
    fn a_temperature_sitting_on_a_threshold_does_not_oscillate() {
        // The actual point of hysteresis. Upstream delays decreases by thirty seconds, which
        // slows this without preventing it -- the fan still flips, just more slowly.
        let mut c = FanController::new(stock(), 3, 10, true);
        c.update(650);
        let mut changes = 0;
        let mut last = c.current().unwrap();
        for i in 0..100 {
            // Wobble either side of the 65C boundary.
            let dc = if i % 2 == 0 { 651 } else { 649 };
            let now = c.update(dc);
            if now != last {
                changes += 1;
                last = now;
            }
        }
        assert_eq!(
            changes, 0,
            "fan duty changed {changes} times while hovering on a threshold"
        );
    }

    #[test]
    fn the_floor_applies_to_any_running_duty() {
        let curve = FanCurve::new(vec![CurvePoint::from_celsius(55, FanDuty::Percent(5))]).unwrap();
        let mut c = FanController::new(curve, 3, 25, true);
        assert_eq!(c.update(600), FanDuty::Percent(25), "floor not applied");
    }

    #[test]
    fn refusing_to_stop_still_cools() {
        // A controller that may not stop the fan must fall back to a running duty, not to
        // something below the speed at which the fan physically turns.
        let mut c = FanController::new(stock(), 3, 10, false);
        let d = c.update(200);
        assert_ne!(d, FanDuty::Off, "fan stopped despite allow_stop = false");
        assert!(d.is_spinning(), "fan set to {d}, which does not spin");
    }

    #[test]
    fn stopping_requires_being_allowed_to() {
        let mut stops = FanController::new(stock(), 3, 10, true);
        assert_eq!(stops.update(200), FanDuty::Off);

        let mut never_stops = FanController::new(stock(), 3, 10, false);
        assert_ne!(never_stops.update(200), FanDuty::Off);
    }

    #[test]
    fn reset_clears_hysteresis_state() {
        let mut c = FanController::new(stock(), 3, 10, true);
        c.update(650);
        assert_eq!(c.current(), Some(FanDuty::Percent(100)));
        c.reset();
        assert_eq!(c.current(), None);
        // With no remembered duty there is nothing to hold, so the curve applies directly.
        assert_eq!(c.update(649), FanDuty::Percent(55));
    }

    #[test]
    fn a_monotonic_sweep_is_stable_under_hysteresis() {
        // Rising then falling must end where it started, with no duty ever exceeding the
        // curve's value for the temperature.
        let curve = stock();
        let mut c = FanController::new(curve.clone(), 3, 10, true);
        for dc in (400..800).step_by(1) {
            let applied = c.update(dc);
            assert!(
                applied.percent() >= curve.duty_for(dc).percent(),
                "at {dc} applied {applied} is below the curve"
            );
        }
        for dc in (400..800).rev() {
            c.update(dc);
        }
        assert_eq!(c.update(400), FanDuty::Off);
    }
}
