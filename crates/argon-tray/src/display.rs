// SPDX-License-Identifier: GPL-3.0-or-later
//! The case display's on/off switch, through argond on D-Bus.
//!
//! argond drives the OLED and polkit decides who may switch it; the tray only shows the switch
//! when argond is driving a display, and flips it.

use argon_device::control::{BUS_NAME, INTERFACE, OBJECT_PATH};

/// Whether the case display is on: `Some(true)` on, `Some(false)` off, `None` when argond is not
/// driving one or cannot be reached -- then there is nothing to offer.
#[must_use]
pub fn state() -> Option<bool> {
    let conn = zbus::blocking::Connection::system().ok()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE).ok()?;
    parse(&proxy.call::<_, _, String>("OledState", &()).ok()?)
}

fn parse(answer: &str) -> Option<bool> {
    match answer {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// What argond says it is monitoring, asked over the bus.
///
/// [`Monitoring::Unknown`] for every failure -- no bus, no service, an older daemon without
/// the method -- because none of those can be told apart from a stopped daemon, and claiming
/// otherwise is the mistake this exists to fix.
#[must_use]
pub fn monitoring() -> crate::view::Monitoring {
    use crate::view::Monitoring;
    let Ok(conn) = zbus::blocking::Connection::system() else {
        return Monitoring::Unknown;
    };
    let Ok(proxy) = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE) else {
        return Monitoring::Unknown;
    };
    let Ok(fields) =
        proxy.call::<_, _, std::collections::HashMap<String, String>>("UpsStatus", &())
    else {
        return Monitoring::Unknown;
    };
    match fields.get("source_config").map(String::as_str) {
        Some("none") => Monitoring::Off,
        Some(src) if !src.is_empty() => Monitoring::Source(src.to_owned()),
        _ => Monitoring::Unknown,
    }
}

/// Switches the case display on or off.
///
/// # Errors
///
/// argond refused, polkit refused, or argond could not be reached.
pub fn set(on: bool) -> Result<(), String> {
    let conn = zbus::blocking::Connection::system().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE)
        .map_err(|e| e.to_string())?;
    proxy
        .call::<_, _, ()>("SetOled", &(on,))
        .map_err(|e| match e {
            zbus::Error::MethodError(_, Some(msg), _) => msg,
            other => other.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn only_on_and_off_offer_a_switch() {
        assert_eq!(parse("on"), Some(true));
        assert_eq!(parse("off"), Some(false));
        assert_eq!(parse("na"), None, "a switch offered with no display");
        assert_eq!(parse("maybe"), None);
    }
}
