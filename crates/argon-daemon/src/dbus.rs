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
use argon_device::control::{ACTION_CPU_CAP, ACTION_POWEROFF_WITH_WAKE, BUS_NAME, OBJECT_PATH};
use std::sync::Mutex;

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
) -> Option<zbus::blocking::Connection> {
    let object = Daemon1 {
        to_ups,
        waiting,
        auth: Box::new(Polkit),
        wake_supported,
        cpu: Mutex::new(Box::new(CpuCap::default())),
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
                action == ACTION_POWEROFF_WITH_WAKE || action == ACTION_CPU_CAP,
                "{action}"
            );
            self.1.lock().unwrap().push(interactive);
            self.0.clone()
        }
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
