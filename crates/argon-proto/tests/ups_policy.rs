// SPDX-License-Identifier: GPL-3.0-or-later
//! The battery policy: when it advises shutdown, and every case where it must not.

use argon_proto::ups::PowerSource::{Battery, Mains};
use argon_proto::ups::policy::{
    Advice, BatteryPolicy, Level, Observation, PolicyConfig, PolicyError,
};
use core::time::Duration;
use proptest::prelude::*;

/// Well past the boot-loop hold.
const UP: Duration = Duration::from_secs(3600);

fn policy() -> BatteryPolicy {
    BatteryPolicy::new(PolicyConfig::default()).unwrap()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "BatteryPolicy::observe takes an Option; None is how a failed read is expressed"
)]
fn obs(percent: u8, source: argon_proto::ups::PowerSource) -> Option<Observation> {
    Some(Observation { percent, source })
}

#[test]
fn the_defaults_are_valid_and_more_conservative_than_the_vendor() {
    let c = PolicyConfig::default();
    assert_eq!(c.validate(), Ok(()));
    // The vendor shuts down at 5%, where a fuel gauge estimate is least trustworthy.
    assert!(
        c.critical_percent > 5,
        "critical threshold {} gives no margin",
        c.critical_percent
    );
    assert!(
        c.confirmations >= 2,
        "a single reading should never be enough"
    );
}

#[test]
fn mains_is_never_a_shutdown_whatever_the_percentage() {
    let mut p = policy();
    for pct in [0, 1, 5, 10, 50, 100] {
        let d = p.observe(obs(pct, Mains), UP);
        assert_eq!(d.level, Level::OnMains);
        assert_eq!(
            d.advice,
            Advice::None,
            "advised shutdown on mains at {pct}%"
        );
    }
}

#[test]
fn levels_step_down_as_the_battery_drains() {
    let mut p = policy();
    assert_eq!(p.observe(obs(80, Battery), UP).level, Level::OnBattery);
    assert_eq!(p.observe(obs(20, Battery), UP).level, Level::Low);
    // First critical reading: not yet confirmed.
    let d = p.observe(obs(10, Battery), UP);
    assert_eq!(d.level, Level::Low);
    assert_eq!(d.advice, Advice::None);
    // Second: confirmed.
    let d = p.observe(obs(9, Battery), UP);
    assert_eq!(d.level, Level::Critical);
    assert_eq!(d.advice, Advice::Shutdown);
    assert_eq!(d.changed_from, Some(Level::Low));
}

#[test]
fn a_single_glitched_reading_never_advises_shutdown() {
    // One corrupt byte on a serial link that can desynchronise must not power the machine off.
    let mut p = policy();
    p.observe(obs(60, Battery), UP);
    let d = p.observe(obs(3, Battery), UP);
    assert_eq!(d.advice, Advice::None, "one reading of 3% advised shutdown");
    let d = p.observe(obs(60, Battery), UP);
    assert_eq!(d.level, Level::OnBattery);
    assert_eq!(d.advice, Advice::None);
}

#[test]
fn critical_advice_repeats_on_every_observation() {
    // The vendor issues shutdown only when its notification text changes, so one missed
    // transition means no shutdown. This is level-triggered instead.
    let mut p = policy();
    p.observe(obs(10, Battery), UP);
    for pct in [9, 8, 8, 7, 7, 6] {
        let d = p.observe(obs(pct, Battery), UP);
        assert_eq!(
            d.advice,
            Advice::Shutdown,
            "no advice at {pct}% while critical"
        );
    }
}

#[test]
fn mains_returning_cancels_immediately() {
    let mut p = policy();
    p.observe(obs(8, Battery), UP);
    assert_eq!(p.observe(obs(8, Battery), UP).advice, Advice::Shutdown);
    let d = p.observe(obs(8, Mains), UP);
    assert_eq!(d.level, Level::OnMains);
    assert_eq!(d.advice, Advice::None);
    assert_eq!(d.changed_from, Some(Level::Critical));
}

#[test]
fn a_small_rise_on_battery_does_not_leave_critical() {
    // Fuel gauges re-estimate, and a reading bouncing just above the threshold should not flap.
    let mut p = policy();
    p.observe(obs(10, Battery), UP);
    p.observe(obs(10, Battery), UP);
    assert_eq!(p.observe(obs(12, Battery), UP).level, Level::Critical);
    assert_eq!(p.observe(obs(14, Battery), UP).level, Level::Critical);
    // Clearing critical + margin steps back to Low, not further.
    assert_eq!(p.observe(obs(15, Battery), UP).level, Level::Low);
    // And Low needs low + margin to clear.
    assert_eq!(p.observe(obs(22, Battery), UP).level, Level::Low);
    assert_eq!(p.observe(obs(25, Battery), UP).level, Level::OnBattery);
}

#[test]
fn a_failed_read_never_advises_shutdown_even_when_critical() {
    let mut p = policy();
    p.observe(obs(8, Battery), UP);
    assert_eq!(p.observe(obs(8, Battery), UP).advice, Advice::Shutdown);
    let d = p.observe(None, UP);
    assert_eq!(d.level, Level::Unknown);
    assert_eq!(d.advice, Advice::None, "advised shutdown without a reading");
}

#[test]
fn readings_resuming_after_a_gap_must_confirm_again() {
    // The bug caught while writing this: a known-critical level used to be restored by the
    // hysteresis branch without consulting the confirmation streak.
    let mut p = policy();
    p.observe(obs(8, Battery), UP);
    p.observe(obs(8, Battery), UP);
    p.observe(None, UP);
    let d = p.observe(obs(8, Battery), UP);
    assert_eq!(
        d.level,
        Level::Low,
        "critical restored from a single reading after a gap"
    );
    assert_eq!(d.advice, Advice::None);
    assert_eq!(p.observe(obs(8, Battery), UP).advice, Advice::Shutdown);
}

#[test]
fn shutdown_is_held_just_after_boot() {
    // Booting on a near-empty battery and shutting straight down loops if power is flapping.
    let mut p = policy();
    p.observe(obs(5, Battery), Duration::from_secs(10));
    let d = p.observe(obs(5, Battery), Duration::from_secs(30));
    assert_eq!(d.level, Level::Critical);
    assert_eq!(
        d.advice,
        Advice::HeldForUptime {
            remaining: Duration::from_secs(90)
        }
    );
    assert_eq!(
        p.observe(obs(5, Battery), Duration::from_secs(120)).advice,
        Advice::Shutdown
    );
}

#[test]
fn invalid_configurations_are_rejected() {
    let base = PolicyConfig::default();
    assert_eq!(
        PolicyConfig {
            critical_percent: 20,
            low_percent: 20,
            ..base
        }
        .validate(),
        Err(PolicyError::CriticalNotBelowLow)
    );
    assert_eq!(
        PolicyConfig {
            low_percent: 150,
            ..base
        }
        .validate(),
        Err(PolicyError::OutOfRange)
    );
    assert_eq!(
        PolicyConfig {
            confirmations: 0,
            ..base
        }
        .validate(),
        Err(PolicyError::NoConfirmations)
    );
    assert_eq!(
        PolicyConfig {
            recover_margin: 0,
            ..base
        }
        .validate(),
        Err(PolicyError::NoMargin)
    );
}

/// An independent statement of when shutdown may be advised, checked against the state
/// machine over arbitrary histories.
fn may_advise_shutdown(history: &[Option<(u8, bool)>], cfg: &PolicyConfig) -> bool {
    // The current observation must be a battery reading, still within critical + margin.
    let Some(Some((pct, on_battery))) = history.last() else {
        return false;
    };
    if !on_battery
        || u16::from(*pct) >= u16::from(cfg.critical_percent) + u16::from(cfg.recover_margin)
    {
        return false;
    }
    // Walk back over the current uninterrupted run of battery readings. Somewhere in it there
    // must be `confirmations` consecutive readings at or below critical.
    let mut streak = 0u8;
    for entry in history.iter().rev() {
        match entry {
            Some((p, true)) => {
                if *p <= cfg.critical_percent {
                    streak += 1;
                    if streak >= cfg.confirmations {
                        return true;
                    }
                } else {
                    streak = 0;
                }
            }
            _ => return false,
        }
    }
    false
}

proptest! {
    #[test]
    fn shutdown_is_only_advised_when_an_independent_check_allows_it(
        history in prop::collection::vec(
            // Biased towards the thresholds. Uniform 0..=100 almost never produces a confirmed
            // critical streak followed by a gap, so a first version of this property passed
            // with the resume-after-gap bug present; only the hand-written test caught it.
            prop::option::weighted(
                0.85,
                (prop_oneof![3 => 0u8..=25, 1 => 0u8..=100], prop::bool::weighted(0.85)),
            ),
            1..60
        )
    ) {
        let cfg = PolicyConfig::default();
        let mut p = BatteryPolicy::new(cfg).unwrap();
        for i in 0..history.len() {
            let o = history[i].map(|(percent, on_battery)| Observation {
                percent,
                source: if on_battery { Battery } else { Mains },
            });
            let d = p.observe(o, UP);
            if d.advice == Advice::Shutdown {
                prop_assert!(
                    may_advise_shutdown(&history[..=i], &cfg),
                    "advised shutdown at step {} without justification: {:?}", i, &history[..=i]
                );
            }
        }
    }

    #[test]
    fn no_shutdown_before_minimum_uptime(
        pcts in prop::collection::vec(0u8..=100, 1..40),
        secs in 0u64..120,
    ) {
        let mut p = policy();
        for pct in pcts {
            let d = p.observe(obs(pct, Battery), Duration::from_secs(secs));
            prop_assert_ne!(d.advice, Advice::Shutdown);
        }
    }
}
