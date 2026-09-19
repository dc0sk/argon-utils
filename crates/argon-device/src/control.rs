// SPDX-License-Identifier: GPL-3.0-or-later
//! The control channel into argond: requests, responses, and where the socket lives.
//!
//! One JSON object per line, one request per connection. Shared by the daemon and `argonctl`,
//! so the two cannot disagree about the protocol.
//!
//! # Who may use it
//!
//! The socket is created by argond, which runs as the `argon` user, with mode `0600`: only
//! `argon` and root can connect. Everything it offers is an action on the machine -- today, a
//! poweroff -- so it is not something an ordinary login should be able to trigger.

use serde::{Deserialize, Serialize};

/// Where argond listens.
pub const SOCKET_PATH: &str = "/run/argon-utils/control.sock";

/// argond's name on the system bus.
pub const BUS_NAME: &str = "org.argonutils.Daemon1";
/// Its object.
pub const OBJECT_PATH: &str = "/org/argonutils/Daemon1";
/// Its interface.
pub const INTERFACE: &str = "org.argonutils.Daemon1";
/// The polkit action guarding a poweroff with a scheduled wake. Must match
/// `packaging/polkit/org.argonutils.policy`.
pub const ACTION_POWEROFF_WITH_WAKE: &str = "org.argonutils.poweroff-with-wake";

/// The polkit action that guards capping the CPU frequency.
pub const ACTION_CPU_CAP: &str = "org.argonutils.cpu-cap";

/// A request to argond.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Set a UPS wake schedule, then power the machine off shortly after.
    PoweroffWithWake {
        /// When to wake, seconds since the unix epoch. Rounded down to the minute.
        at_unix: u64,
    },
}

/// argond's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// Done: the wake is set and read back, and the poweroff is scheduled.
    PoweroffScheduled {
        /// The wake as the UPS holds it, seconds since the unix epoch.
        wake_unix: u64,
        /// When the machine powers off, seconds since the unix epoch.
        poweroff_unix: u64,
    },
    /// Refused or failed, with the reason, and what state it left behind.
    Error {
        /// What went wrong, in terms an operator can act on.
        message: String,
    },
}

impl Response {
    /// An error response.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

/// Encodes a message as one line.
///
/// # Errors
///
/// Fails only if serialisation fails, which these types cannot cause.
pub fn to_line<T: Serialize>(msg: &T) -> serde_json::Result<String> {
    let mut s = serde_json::to_string(msg)?;
    s.push('\n');
    Ok(s)
}

/// Decodes one line.
///
/// # Errors
///
/// Fails on malformed input.
pub fn from_line<'a, T: Deserialize<'a>>(line: &'a str) -> serde_json::Result<T> {
    serde_json::from_str(line.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_responses_round_trip_as_single_lines() {
        let req = Request::PoweroffWithWake {
            at_unix: 1_789_800_000,
        };
        let line = to_line(&req).unwrap();
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        assert_eq!(from_line::<Request>(&line).unwrap(), req);

        for resp in [
            Response::PoweroffScheduled {
                wake_unix: 1,
                poweroff_unix: 2,
            },
            Response::error("nope"),
        ] {
            let line = to_line(&resp).unwrap();
            assert_eq!(from_line::<Response>(&line).unwrap(), resp);
        }
    }

    #[test]
    fn the_wire_form_is_stable() {
        // Pinned: a change here breaks every argonctl talking to an older argond, and must be
        // a deliberate protocol change rather than a side effect of a rename.
        assert_eq!(
            to_line(&Request::PoweroffWithWake { at_unix: 60 }).unwrap(),
            "{\"poweroff_with_wake\":{\"at_unix\":60}}\n"
        );
    }

    #[test]
    fn the_action_named_in_code_is_the_one_the_policy_file_defines() {
        // A mismatch is silent at runtime: polkit answers "no" for an action it has never heard
        // of, and the feature simply never works.
        let policy = include_str!("../../../packaging/polkit/org.argonutils.policy");
        assert!(policy.contains(&format!("<action id=\"{ACTION_POWEROFF_WITH_WAKE}\">")));
        assert!(policy.contains(&format!("<action id=\"{ACTION_CPU_CAP}\">")));
        let dbus = include_str!("../../../packaging/dbus/org.argonutils.Daemon1.conf");
        assert!(dbus.contains(&format!("own=\"{BUS_NAME}\"")));
        assert!(dbus.contains(&format!("send_interface=\"{INTERFACE}\"")));
    }

    #[test]
    fn garbage_is_an_error_not_a_default() {
        assert!(from_line::<Request>("{\"reboot\":{}}").is_err());
        assert!(from_line::<Request>("poweroff").is_err());
    }
}
