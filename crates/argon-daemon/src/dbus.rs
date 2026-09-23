// SPDX-License-Identifier: GPL-3.0-or-later
//! The D-Bus service: `org.argonutils.Daemon1` on the system bus.
//!
//! This is how programs running as an ordinary user -- the tray icon -- ask argond to act. The
//! control socket is root-only; here, **polkit** decides per call, with the same defaults the
//! desktop's own power-off has: an active local session may, anyone else needs an administrator.
//!
//! The handlers are thin. Authorization is behind [`Authorize`], so the decisions can be tested
//! without a bus, and the actual request goes through the same relay to the UPS thread as the
//! control socket's -- one path to the hardware, not two.

use crate::control::{Message, relay};
use argon_device::control::{Request, Response};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use zbus::message::Header;
use zbus::zvariant::Value;

use crate::cpu_cap::{CapControl, CpuCap};
use crate::oled::OledControl;
use crate::ups::Latest;
use argon_device::control::{
    ACTION_CPU_CAP, ACTION_OLED, ACTION_POWEROFF_WITH_WAKE, BUS_NAME, OBJECT_PATH,
};
use argon_device::status::{UpsStatus, source_of};
use std::sync::Mutex;
use std::time::SystemTime;

/// What polkit said about a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Allowed.
    Yes,
    /// Allowed after authentication.
    Challenge,
    /// Not allowed.
    No,
}

/// Decides whether a caller may do something.
pub trait Authorize: Send + Sync {
    /// Asks about `sender` (a unique bus name) and `action`. With `interactive`, the caller's
    /// authentication agent may prompt.
    ///
    /// # Errors
    ///
    /// The question could not be asked.
    fn check(&self, sender: &str, action: &str, interactive: bool) -> Result<Verdict, String>;
}

/// The real authority: polkit, over the system bus.
///
/// Each check opens its own connection. The call it makes goes to polkitd over the system bus,
/// and making it on the connection that is serving the very method being checked would wait on
/// a reply that connection cannot dispatch while it is busy. Checks are rare; a connection each
/// costs nothing that matters.
pub struct Polkit;

impl Authorize for Polkit {
    fn check(&self, sender: &str, action: &str, interactive: bool) -> Result<Verdict, String> {
        let conn = zbus::blocking::Connection::system().map_err(|e| e.to_string())?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.PolicyKit1",
            "/org/freedesktop/PolicyKit1/Authority",
            "org.freedesktop.PolicyKit1.Authority",
        )
        .map_err(|e| e.to_string())?;
        let mut subject_details: HashMap<&str, Value<'_>> = HashMap::new();
        subject_details.insert("name", Value::from(sender));
        let subject = ("system-bus-name", subject_details);
        let details: HashMap<&str, &str> = HashMap::new();
        // 1 = AllowUserInteraction. argond may ask about other identities for this action
        // because the action file names `unix-user:argon` as its owner.
        let flags: u32 = u32::from(interactive);
        let (authorized, challenge, _): (bool, bool, HashMap<String, String>) = proxy
            .call("CheckAuthorization", &(subject, action, details, flags, ""))
            .map_err(|e| e.to_string())?;
        Ok(if authorized {
            Verdict::Yes
        } else if challenge {
            Verdict::Challenge
        } else {
            Verdict::No
        })
    }
}

/// The object on the bus.
pub struct Daemon1 {
    to_ups: Sender<Message>,
    waiting: Arc<AtomicBool>,
    auth: Box<dyn Authorize>,
    /// Whether the battery argond watches can wake the machine. The ONE UP's cannot.
    wake_supported: bool,
    /// The CPU frequency cap. Dropped with the object, which lifts any cap still in force.
    cpu: Mutex<Box<dyn CapControl>>,
    /// The case display's on/off switch, shared with the OLED thread.
    oled: Arc<OledControl>,
    /// The last reading the monitoring thread published.
    latest: Latest,
    /// What the control loop last did with the fan.
    fan: Arc<Mutex<crate::exporter::State>>,
    /// The configured UPS source, so a caller with no reading can be told *why*.
    ups_source: String,
}

// The interface macro fixes these signatures: a method takes `&self` whether it needs it or
// not, and the message header by value.
#[allow(clippy::unused_self, clippy::needless_pass_by_value)]
#[zbus::interface(name = "org.argonutils.Daemon1")]
impl Daemon1 {
    /// argond's version. Harmless: for checking the service is there.
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_owned()
    }

    /// Whether the caller may power off with a wake: "yes", "challenge", "no", or "na" when
    /// this hardware has no wake -- the shape of logind's `CanPowerOff`. Only asks polkit;
    /// changes nothing.
    fn can_poweroff_with_wake(&self, #[zbus(header)] header: Header<'_>) -> String {
        can(self.auth.as_ref(), &sender_of(&header), self.wake_supported).to_owned()
    }

    /// Sets a UPS wake at `at_unix`, reads it back, then powers off a minute later. Returns the
    /// wake as the UPS holds it and the poweroff time, both unix seconds.
    fn poweroff_with_wake(
        &self,
        #[zbus(header)] header: Header<'_>,
        at_unix: u64,
    ) -> zbus::fdo::Result<(u64, u64)> {
        poweroff(self.auth.as_ref(), &sender_of(&header), at_unix, |req| {
            relay(req, &self.to_ups, &self.waiting)
        })
    }

    /// What argond last read from the battery, as `key=value` pairs. Reports only.
    ///
    /// This is how `argonctl` and anything else in a user session can see the battery without
    /// opening the device: argond holds the UPS port, and handing a login the group that owns
    /// it would put a second writer on a link that has no arbitration.
    ///
    /// Always answers. `available=no` means argond is not monitoring a battery, or has not
    /// completed a reading yet -- which is a different thing from the service being absent, and
    /// the caller can tell the two apart because an absent service cannot reply at all.
    fn ups_status(&self) -> HashMap<String, String> {
        let latest = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut f = status_fields(latest.as_ref(), SystemTime::now());
        // So a caller with no reading can say *why*: "monitoring is off" and "no reading yet"
        // look identical otherwise, and the first is a configuration choice, not a fault.
        f.insert("source_config".to_owned(), self.ups_source.clone());
        f
    }

    /// What argond has the fan doing, as `key=value` pairs. Reports only.
    ///
    /// The point is the difference between intent and fact: `argonctl fan` alone can only say
    /// what the curve *would* choose, which is not what the fan is doing when a daemon is in
    /// charge of it. Keys: `driving`, `duty_percent`, `temperature_c`, `age_s`.
    fn fan_state(&self) -> HashMap<String, String> {
        let state = self
            .fan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fan_fields(&state, SystemTime::now())
    }

    /// The case display: "on", "off", or "na" when argond is not driving one. Changes nothing.
    fn oled_state(&self) -> String {
        self.oled.state().to_owned()
    }

    /// Switches the case display on (`true`) or off. Never prompts, like the CPU cap.
    fn set_oled(&self, #[zbus(header)] header: Header<'_>, on: bool) -> zbus::fdo::Result<()> {
        set_oled(self.auth.as_ref(), &sender_of(&header), on, &self.oled)
    }

    /// Caps the CPU at its lowest frequency (`true`), or puts back the limit in force before
    /// (`false`). For the lid agent while a laptop lid is closed. Never prompts: the agent asks
    /// with nobody looking at the screen.
    fn set_cpu_cap(
        &self,
        #[zbus(header)] header: Header<'_>,
        capped: bool,
    ) -> zbus::fdo::Result<()> {
        let mut cpu = self
            .cpu
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set_cpu_cap(
            self.auth.as_ref(),
            &sender_of(&header),
            capped,
            cpu.as_mut(),
        )
    }
}

/// `SetOled`, without the bus.
fn set_oled(
    auth: &dyn Authorize,
    sender: &str,
    on: bool,
    oled: &OledControl,
) -> zbus::fdo::Result<()> {
    match auth.check(sender, ACTION_OLED, false) {
        Ok(Verdict::Yes) => {}
        Ok(Verdict::Challenge | Verdict::No) => {
            return Err(zbus::fdo::Error::AccessDenied(
                "not authorised by polkit to switch the case display".into(),
            ));
        }
        Err(e) => {
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "could not check authorisation, so refusing: {e}"
            )));
        }
    }
    oled.set(on).map_err(zbus::fdo::Error::Failed)
}

/// `SetCpuCap`, without the bus.
fn set_cpu_cap(
    auth: &dyn Authorize,
    sender: &str,
    capped: bool,
    cpu: &mut dyn CapControl,
) -> zbus::fdo::Result<()> {
    match auth.check(sender, ACTION_CPU_CAP, false) {
        Ok(Verdict::Yes) => {}
        Ok(Verdict::Challenge | Verdict::No) => {
            return Err(zbus::fdo::Error::AccessDenied(
                "not authorised by polkit to change the CPU frequency limit".into(),
            ));
        }
        Err(e) => {
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "could not check authorisation, so refusing: {e}"
            )));
        }
    }
    let result = if capped { cpu.cap() } else { cpu.lift() };
    result.map_err(zbus::fdo::Error::Failed)?;
    eprintln!(
        "argond: cpu: {} at {sender}'s request",
        if capped {
            "capped at its lowest frequency"
        } else {
            "cap lifted"
        }
    );
    Ok(())
}

/// What the control loop last did with the fan, as flat pairs.
///
/// `driving: no` is not a failure: in read-only mode, or with no MCU, the daemon deliberately
/// reports the fan without touching it. Saying so is the difference between a caller
/// concluding "nothing is in control" and "this daemon is not the thing in control".
fn fan_fields(state: &crate::exporter::State, now: SystemTime) -> HashMap<String, String> {
    let mut f = HashMap::new();
    let Some(updated) = state.updated_unix else {
        f.insert("available".to_owned(), "no".to_owned());
        return f;
    };
    f.insert("available".to_owned(), "yes".to_owned());
    f.insert(
        "driving".to_owned(),
        if state.fan_driving { "yes" } else { "no" }.to_owned(),
    );
    f.insert(
        "duty_percent".to_owned(),
        state
            .fan_duty_percent
            .map_or_else(String::new, |d| d.to_string()),
    );
    f.insert(
        "temperature_c".to_owned(),
        state
            .cpu_decicelsius
            .map_or_else(String::new, |d| format!("{}.{}", d / 10, (d % 10).abs())),
    );
    f.insert(
        "sensor_failures".to_owned(),
        state.sensor_failures.to_string(),
    );
    let age = unix(now).saturating_sub(updated);
    f.insert("age_s".to_owned(), age.to_string());
    f
}

/// The status as flat pairs for the bus.
///
/// `a{ss}` rather than a struct: a reader that does not know a key ignores it, so a later
/// version can report more without breaking an older `argonctl`. Times are unix seconds, and
/// `age_s` is included because a caller cannot otherwise tell a fresh reading from one left
/// behind by a thread that died half an hour ago.
fn status_fields(latest: Option<&UpsStatus>, now: SystemTime) -> HashMap<String, String> {
    let mut f = HashMap::new();
    let Some(s) = latest else {
        f.insert("available".to_owned(), "no".to_owned());
        return f;
    };
    f.insert("available".to_owned(), "yes".to_owned());
    f.insert("level".to_owned(), s.level.clone());
    f.insert("source".to_owned(), source_of(&s.level).to_owned());
    f.insert(
        "percent".to_owned(),
        s.percent.map_or_else(String::new, |p| p.to_string()),
    );
    f.insert("updated_unix".to_owned(), unix(s.updated).to_string());
    f.insert(
        "age_s".to_owned(),
        now.duration_since(s.updated)
            .map_or_else(|_| "0".to_owned(), |d| d.as_secs().to_string()),
    );
    f.insert(
        "shutdown_at_unix".to_owned(),
        s.shutdown_at
            .map_or_else(String::new, |t| unix(t).to_string()),
    );
    f
}

/// Seconds since the epoch, saturating at zero for a time before it.
fn unix(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn sender_of(header: &Header<'_>) -> String {
    header.sender().map(ToString::to_string).unwrap_or_default()
}

/// `CanPoweroffWithWake`, without the bus.
fn can(auth: &dyn Authorize, sender: &str, wake_supported: bool) -> &'static str {
    if !wake_supported {
        return "na";
    }
    match auth.check(sender, ACTION_POWEROFF_WITH_WAKE, false) {
        Ok(Verdict::Yes) => "yes",
        Ok(Verdict::Challenge) => "challenge",
        // A failed check is a "no": the one wrong answer that must not happen here is "yes".
        Ok(Verdict::No) | Err(_) => "no",
    }
}

/// `PoweroffWithWake`, without the bus.
fn poweroff(
    auth: &dyn Authorize,
    sender: &str,
    at_unix: u64,
    relay: impl FnOnce(Request) -> Response,
) -> zbus::fdo::Result<(u64, u64)> {
    match auth.check(sender, ACTION_POWEROFF_WITH_WAKE, true) {
        Ok(Verdict::Yes) => {}
        Ok(Verdict::Challenge | Verdict::No) => {
            return Err(zbus::fdo::Error::AccessDenied(
                "not authorised by polkit to power off with a wake".into(),
            ));
        }
        Err(e) => {
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "could not check authorisation, so refusing: {e}"
            )));
        }
    }
    match relay(Request::PoweroffWithWake { at_unix }) {
        Response::PoweroffScheduled {
            wake_unix,
            poweroff_unix,
        } => Ok((wake_unix, poweroff_unix)),
        Response::Error { message } => Err(zbus::fdo::Error::Failed(message)),
    }
}

/// Connects to the system bus, claims the name and serves the object.
///
/// The returned connection must be kept: dropping it takes the service off the bus. `None` --
/// after saying why -- when there is no system bus, or the D-Bus policy does not let argond own
/// the name, which is the case when it is run by hand as a user the policy does not name.
pub fn serve(
    to_ups: Sender<Message>,
    waiting: Arc<AtomicBool>,
    wake_supported: bool,
    oled: Arc<OledControl>,
    latest: Latest,
    fan: Arc<Mutex<crate::exporter::State>>,
    ups_source: String,
) -> Option<zbus::blocking::Connection> {
    let object = Daemon1 {
        to_ups,
        waiting,
        auth: Box::new(Polkit),
        wake_supported,
        cpu: Mutex::new(Box::new(CpuCap::default())),
        oled,
        latest,
        fan,
        ups_source,
    };
    let built = zbus::blocking::connection::Builder::system()
        .and_then(|b| b.name(BUS_NAME))
        .and_then(|b| b.serve_at(OBJECT_PATH, object))
        .and_then(zbus::blocking::connection::Builder::build);
    match built {
        Ok(conn) => {
            eprintln!("argond: dbus: serving {BUS_NAME} at {OBJECT_PATH}");
            Some(conn)
        }
        Err(e) => {
            eprintln!("argond: dbus: not on the system bus ({e}); the tray cannot ask for actions");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Answers with a fixed verdict, and records whether it was asked interactively.
    struct Fixed(Result<Verdict, String>, std::sync::Mutex<Vec<bool>>);

    impl Fixed {
        fn new(v: Result<Verdict, String>) -> Self {
            Self(v, std::sync::Mutex::new(Vec::new()))
        }
    }

    impl Authorize for Fixed {
        fn check(&self, _: &str, action: &str, interactive: bool) -> Result<Verdict, String> {
            assert!(
                [ACTION_POWEROFF_WITH_WAKE, ACTION_CPU_CAP, ACTION_OLED].contains(&action),
                "{action}"
            );
            self.1.lock().unwrap().push(interactive);
            self.0.clone()
        }
    }

    fn at(unix: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(unix)
    }

    fn status(level: &str, percent: Option<u8>, shutdown_at: Option<u64>) -> UpsStatus {
        UpsStatus {
            updated: at(1_000),
            level: level.to_owned(),
            percent,
            shutdown_at: shutdown_at.map(at),
        }
    }

    fn fan_state(duty: Option<u8>, driving: bool, updated: Option<u64>) -> crate::exporter::State {
        crate::exporter::State {
            cpu_decicelsius: Some(535),
            sensor_failures: 0,
            fan_duty_percent: duty,
            fan_driving: driving,
            updated_unix: updated,
        }
    }

    #[test]
    fn the_fan_state_says_what_the_daemon_has_it_doing() {
        let f = fan_fields(&fan_state(Some(0), true, Some(1_000)), at(1_003));
        assert_eq!(f["available"], "yes");
        assert_eq!(f["driving"], "yes");
        assert_eq!(f["duty_percent"], "0");
        assert_eq!(f["temperature_c"], "53.5");
        assert_eq!(f["age_s"], "3");
    }

    #[test]
    fn not_driving_is_reported_as_such_rather_than_as_no_information() {
        // Read-only mode, or no MCU: the daemon reports the fan without setting it. A caller
        // must be able to tell that apart from "nothing is in control".
        let f = fan_fields(&fan_state(Some(30), false, Some(1_000)), at(1_000));
        assert_eq!(f["available"], "yes");
        assert_eq!(f["driving"], "no");
    }

    #[test]
    fn before_the_first_iteration_there_is_nothing_to_report() {
        let f = fan_fields(&fan_state(None, true, None), at(1_000));
        assert_eq!(f["available"], "no");
        assert!(!f.contains_key("duty_percent"), "invented a duty");
    }

    #[test]
    fn a_reading_is_reported_with_the_source_its_level_implies() {
        let f = status_fields(Some(&status("on-battery", Some(64), None)), at(1_005));
        assert_eq!(f["available"], "yes");
        assert_eq!(f["level"], "on-battery");
        assert_eq!(f["source"], "battery", "a battery level reported as mains");
        assert_eq!(f["percent"], "64");
        assert_eq!(f["updated_unix"], "1000");
        assert_eq!(f["age_s"], "5");
        assert_eq!(f["shutdown_at_unix"], "");
    }

    #[test]
    fn nothing_read_yet_is_said_plainly_rather_than_as_a_zero() {
        let f = status_fields(None, at(1_000));
        assert_eq!(f["available"], "no");
        assert!(!f.contains_key("percent"), "invented a percentage");
        assert!(!f.contains_key("level"));
    }

    #[test]
    fn a_failed_read_leaves_the_percentage_empty_not_zero() {
        // The level still says what the policy concluded; the charge is simply unknown.
        let f = status_fields(Some(&status("unknown", None, None)), at(1_000));
        assert_eq!(f["percent"], "");
        assert_eq!(f["source"], "unknown");
    }

    #[test]
    fn a_pending_poweroff_is_reported() {
        let f = status_fields(Some(&status("critical", Some(4), Some(1_300))), at(1_000));
        assert_eq!(f["shutdown_at_unix"], "1300");
        assert_eq!(f["source"], "battery");
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_make_the_age_wrap() {
        // NTP stepping the clock back must not turn a fresh reading into a 136-year-old one.
        let f = status_fields(Some(&status("on-mains", Some(90), None)), at(900));
        assert_eq!(f["age_s"], "0");
    }

    #[test]
    fn can_reports_polkit_and_never_turns_an_error_into_yes() {
        assert_eq!(can(&Fixed::new(Ok(Verdict::Yes)), ":1.5", true), "yes");
        assert_eq!(
            can(&Fixed::new(Ok(Verdict::Challenge)), ":1.5", true),
            "challenge"
        );
        assert_eq!(can(&Fixed::new(Ok(Verdict::No)), ":1.5", true), "no");
        assert_eq!(
            can(&Fixed::new(Err("no polkitd".into())), ":1.5", true),
            "no"
        );
    }

    /// Records what it was asked to do.
    #[derive(Default)]
    struct FakeCap(Vec<bool>);

    impl CapControl for FakeCap {
        fn cap(&mut self) -> Result<(), String> {
            self.0.push(true);
            Ok(())
        }
        fn lift(&mut self) -> Result<(), String> {
            self.0.push(false);
            Ok(())
        }
    }

    #[test]
    fn the_cpu_cap_is_set_and_lifted_only_for_an_authorised_caller() {
        let mut cpu = FakeCap::default();
        let auth = Fixed::new(Ok(Verdict::Yes));
        set_cpu_cap(&auth, ":1.5", true, &mut cpu).unwrap();
        set_cpu_cap(&auth, ":1.5", false, &mut cpu).unwrap();
        assert_eq!(cpu.0, vec![true, false]);
        assert_eq!(
            *auth.1.lock().unwrap(),
            vec![false, false],
            "the cap check prompted"
        );
        for verdict in [Ok(Verdict::No), Ok(Verdict::Challenge), Err("gone".into())] {
            let mut cpu = FakeCap::default();
            assert!(set_cpu_cap(&Fixed::new(verdict), ":1.5", true, &mut cpu).is_err());
            assert!(
                cpu.0.is_empty(),
                "an unauthorised caller changed the CPU limit"
            );
        }
    }

    #[test]
    fn the_display_is_switched_only_for_an_authorised_caller_and_never_prompts() {
        let oled = OledControl::new(None);
        oled.mark_available_for_tests();
        let auth = Fixed::new(Ok(Verdict::Yes));
        set_oled(&auth, ":1.5", false, &oled).unwrap();
        assert_eq!(oled.state(), "off");
        assert_eq!(*auth.1.lock().unwrap(), vec![false], "the check prompted");
        for verdict in [Ok(Verdict::No), Ok(Verdict::Challenge), Err("gone".into())] {
            assert!(set_oled(&Fixed::new(verdict), ":1.5", true, &oled).is_err());
            assert_eq!(
                oled.state(),
                "off",
                "an unauthorised caller switched the display"
            );
        }
    }

    #[test]
    fn hardware_without_a_wake_says_so_whatever_polkit_would_say() {
        // The ONE UP: "na", so the tray does not offer what cannot be done.
        assert_eq!(can(&Fixed::new(Ok(Verdict::Yes)), ":1.5", false), "na");
    }

    #[test]
    fn can_never_prompts() {
        let auth = Fixed::new(Ok(Verdict::Yes));
        let _ = can(&auth, ":1.5", true);
        assert_eq!(
            *auth.1.lock().unwrap(),
            vec![false],
            "a query asked interactively"
        );
    }

    #[test]
    fn an_authorised_caller_is_relayed_and_gets_the_times() {
        let relayed = Cell::new(false);
        let got = poweroff(&Fixed::new(Ok(Verdict::Yes)), ":1.5", 1_000, |req| {
            relayed.set(true);
            assert_eq!(req, Request::PoweroffWithWake { at_unix: 1_000 });
            Response::PoweroffScheduled {
                wake_unix: 960,
                poweroff_unix: 60,
            }
        });
        assert_eq!(got.unwrap(), (960, 60));
        assert!(relayed.get());
    }

    #[test]
    fn refusals_and_failed_checks_never_reach_the_ups() {
        for verdict in [
            Ok(Verdict::No),
            Ok(Verdict::Challenge),
            Err("polkitd gone".into()),
        ] {
            let reached = Cell::new(false);
            let got = poweroff(&Fixed::new(verdict.clone()), ":1.5", 1_000, |_| {
                reached.set(true);
                Response::PoweroffScheduled {
                    wake_unix: 0,
                    poweroff_unix: 0,
                }
            });
            assert!(
                matches!(got, Err(zbus::fdo::Error::AccessDenied(_))),
                "{verdict:?} gave {got:?}"
            );
            assert!(!reached.get(), "{verdict:?} reached the UPS thread");
        }
    }

    #[test]
    fn the_check_for_the_real_action_is_interactive() {
        // So an authentication agent can prompt where the policy says auth_admin.
        let auth = Fixed::new(Ok(Verdict::Yes));
        let _ = poweroff(&auth, ":1.5", 1, |_| Response::error("x"));
        assert_eq!(*auth.1.lock().unwrap(), vec![true]);
    }

    #[test]
    fn a_ups_thread_refusal_comes_back_as_a_failure_with_its_reason() {
        let got = poweroff(&Fixed::new(Ok(Verdict::Yes)), ":1.5", 1, |_| {
            Response::error("the wake would be only 60 s away")
        });
        match got {
            Err(zbus::fdo::Error::Failed(m)) => assert!(m.contains("60 s"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}
