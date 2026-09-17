// SPDX-License-Identifier: GPL-3.0-or-later
//! The status file, and which changes deserve a notification.

use argon_device::status::{STALE_AFTER, UpsStatus, Urgency, Watcher, level_name, notice};
use argon_proto::ups::policy::Level;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const T0: u64 = 1_800_000_000;

fn at(s: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(s)
}

fn status(level: &str, percent: Option<u8>, shutdown_at: Option<u64>) -> UpsStatus {
    UpsStatus {
        updated: at(T0),
        level: level.to_owned(),
        percent,
        shutdown_at: shutdown_at.map(at),
    }
}

fn fmt(t: SystemTime) -> String {
    format!("T+{}", t.duration_since(at(T0)).unwrap().as_secs())
}

fn say(prev: Option<&UpsStatus>, cur: &UpsStatus) -> Option<(Urgency, String)> {
    notice(prev, cur, &fmt).map(|n| (n.urgency, n.text))
}

#[test]
fn the_file_round_trips() {
    for s in [
        status("on-mains", Some(94), None),
        status("critical", Some(9), Some(T0 + 120)),
        status("unknown", None, None),
    ] {
        assert_eq!(UpsStatus::parse(&s.to_text()).unwrap(), s);
    }
}

#[test]
fn an_unknown_version_is_refused_but_unknown_keys_are_not() {
    // A newer daemon may add fields; an older agent must keep working. A different version
    // means the meaning changed, and guessing would be worse than refusing.
    let mut text = status("low", Some(18), None).to_text();
    text.push_str("future_field=whatever\n");
    assert!(UpsStatus::parse(&text).is_ok());
    let text = text.replace("version=1", "version=2");
    assert!(UpsStatus::parse(&text).is_err());
}

#[test]
fn malformed_values_are_errors() {
    let text = status("low", Some(18), None)
        .to_text()
        .replace("percent=18", "percent=lots");
    assert!(UpsStatus::parse(&text).is_err());
    assert!(UpsStatus::parse("").is_err());
}

#[test]
fn writes_are_atomic_and_leave_no_temp_file() {
    let dir = std::env::temp_dir().join(format!("argon-status-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ups.state");
    let s = status("on-battery", Some(55), None);
    s.write_atomic(&path).unwrap();
    assert_eq!(
        UpsStatus::parse(&std::fs::read_to_string(&path).unwrap()).unwrap(),
        s
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        leftovers.len(),
        1,
        "temporary file left behind: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn level_names_are_stable() {
    // These strings are an interface between the daemon and the agent.
    assert_eq!(level_name(Level::OnMains), "on-mains");
    assert_eq!(level_name(Level::OnBattery), "on-battery");
    assert_eq!(level_name(Level::Low), "low");
    assert_eq!(level_name(Level::Critical), "critical");
    assert_eq!(level_name(Level::Unknown), "unknown");
}

#[test]
fn nothing_is_said_at_login_when_all_is_well() {
    // A notification at every login saying "on mains" trains people to dismiss them.
    assert_eq!(say(None, &status("on-mains", Some(94), None)), None);
    assert_eq!(say(None, &status("on-battery", Some(80), None)), None);
}

#[test]
fn a_problem_already_present_at_login_is_reported() {
    assert!(matches!(
        say(None, &status("low", Some(15), None)),
        Some((Urgency::Normal, _))
    ));
    assert!(matches!(
        say(None, &status("critical", Some(8), None)),
        Some((Urgency::Critical, _))
    ));
}

#[test]
fn a_scheduled_shutdown_is_critical_and_says_how_to_cancel() {
    let prev = status("low", Some(11), None);
    let cur = status("critical", Some(9), Some(T0 + 120));
    let (urgency, text) = say(Some(&prev), &cur).unwrap();
    assert_eq!(urgency, Urgency::Critical);
    assert!(text.contains("T+120"), "no shutdown time: {text}");
    assert!(
        text.contains("Restore mains power to cancel"),
        "no way out given: {text}"
    );
}

#[test]
fn a_scheduled_shutdown_already_in_place_at_login_is_still_reported() {
    // Logging in during the countdown is exactly when you need to know.
    let cur = status("critical", Some(9), Some(T0 + 120));
    let (urgency, text) = say(None, &cur).unwrap();
    assert_eq!(urgency, Urgency::Critical);
    assert!(text.contains("powering off"), "{text}");
}

#[test]
fn a_cancelled_shutdown_is_reported_once() {
    let prev = status("critical", Some(9), Some(T0 + 120));
    let cur = status("on-mains", Some(9), None);
    let (_, text) = say(Some(&prev), &cur).unwrap();
    assert!(text.contains("shutdown cancelled"), "{text}");
    assert_eq!(say(Some(&cur), &cur), None, "repeated itself");
}

#[test]
fn transitions_are_reported_and_steady_state_is_not() {
    let mains = status("on-mains", Some(90), None);
    let batt = status("on-battery", Some(90), None);
    let low = status("low", Some(19), None);
    assert!(
        say(Some(&mains), &batt)
            .unwrap()
            .1
            .contains("Running on battery")
    );
    assert!(say(Some(&batt), &low).unwrap().1.contains("Battery low"));
    assert!(
        say(Some(&low), &mains)
            .unwrap()
            .1
            .contains("Mains power restored")
    );
    assert_eq!(say(Some(&low), &low), None);
}

#[test]
fn a_status_nobody_updates_is_reported_once() {
    // Written against the first version, which suppressed this every time: when the daemon
    // stops writing, previous and current are the same file with the same timestamp.
    let fresh = status("on-mains", Some(90), None);
    let mut w = Watcher::new();
    assert_eq!(w.observe(Some(fresh.clone()), at(T0 + 1), &fmt), None);

    let later = at(T0) + STALE_AFTER + Duration::from_secs(10);
    let n = w
        .observe(Some(fresh.clone()), later, &fmt)
        .expect("stale status not reported");
    assert!(n.text.contains("stopped updating"), "{}", n.text);
    assert_eq!(
        w.observe(Some(fresh), later + Duration::from_secs(2), &fmt),
        None,
        "repeated"
    );
}

#[test]
fn a_resumed_daemon_can_be_reported_stale_again_later() {
    let mut w = Watcher::new();
    w.observe(Some(status("on-mains", Some(90), None)), at(T0 + 1), &fmt);
    let stale_time = at(T0) + STALE_AFTER + Duration::from_secs(10);
    assert!(
        w.observe(Some(status("on-mains", Some(90), None)), stale_time, &fmt)
            .is_some()
    );

    // The daemon comes back and writes a fresh status...
    let resumed = UpsStatus {
        updated: stale_time,
        ..status("on-mains", Some(91), None)
    };
    w.observe(
        Some(resumed.clone()),
        stale_time + Duration::from_secs(1),
        &fmt,
    );
    // ...and later stops again: that is a new event and deserves a new notice.
    let again = stale_time + STALE_AFTER + Duration::from_secs(10);
    assert!(w.observe(Some(resumed), again, &fmt).is_some());
}

#[test]
fn no_file_means_no_daemon_and_says_nothing() {
    let mut w = Watcher::new();
    assert_eq!(w.observe(None, at(T0), &fmt), None);
}

#[test]
fn the_watcher_reports_a_scheduled_shutdown_through_transitions() {
    let mut w = Watcher::new();
    let now = at(T0 + 1);
    assert_eq!(
        w.observe(Some(status("on-battery", Some(12), None)), now, &fmt),
        None
    );
    let n = w
        .observe(Some(status("critical", Some(9), Some(T0 + 120))), now, &fmt)
        .unwrap();
    assert_eq!(n.urgency, Urgency::Critical);
    let n = w
        .observe(Some(status("on-mains", Some(9), None)), now, &fmt)
        .unwrap();
    assert!(n.text.contains("shutdown cancelled"), "{}", n.text);
}
