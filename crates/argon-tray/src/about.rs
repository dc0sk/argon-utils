// SPDX-License-Identifier: GPL-3.0-or-later
//! What the tray's "About" menu shows: which of our components are running, and where to
//! find the project.
//!
//! Versions are asked of the components themselves rather than assumed from the tray's own:
//! `argond` is a separate process that a package upgrade may have left at a different version
//! until it restarts, and `argonctl` is a separate binary that may not be installed at all.
//! Reporting the tray's version three times would look like agreement without being evidence
//! of any.
//!
//! Third-party versions are deliberately absent. They are visible to `apt` and `cargo`, and a
//! panel menu is not where a dependency audit belongs.

/// Where the project lives.
pub const REPO_URL: &str = "https://github.com/dc0sk/argon-utils";
/// Where to donate, from the project's own funding file.
pub const DONATE_URL: &str = "https://www.paypal.com/donate/?hosted_button_id=WY9U4MQ3ZAQWC";

/// One component and the version it reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Program name.
    pub name: &'static str,
    /// What it reports, or `None` when it could not be asked.
    pub version: Option<String>,
}

/// How a component's line reads in the menu.
#[must_use]
pub fn line(c: &Component) -> String {
    match &c.version {
        Some(v) => format!("{}  {v}", c.name),
        // Not "unknown": the reason is the useful part, and for argond it is the answer to
        // the question someone opening this menu is most likely asking.
        None if c.name == "argond" => format!("{}  not answering on the bus", c.name),
        None => format!("{}  not installed", c.name),
    }
}

/// The components, in the order they are shown.
#[must_use]
pub fn components(daemon: Option<String>, cli: Option<String>) -> Vec<Component> {
    vec![
        Component {
            name: "argon-tray",
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        },
        Component {
            name: "argond",
            version: daemon,
        },
        Component {
            name: "argonctl",
            version: cli,
        },
    ]
}

/// Asks `argond` its version over the bus.
#[must_use]
pub fn daemon_version() -> Option<String> {
    use argon_device::control::{BUS_NAME, INTERFACE, OBJECT_PATH};
    let conn = zbus::blocking::Connection::system().ok()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE).ok()?;
    proxy.call::<_, _, String>("Version", &()).ok()
}

/// Asks the installed `argonctl` its version.
#[must_use]
pub fn cli_version() -> Option<String> {
    let out = std::process::Command::new("argonctl")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    // `clap` prints "argonctl 0.1.33"; take the version, not the whole line.
    text.split_whitespace().nth(1).map(ToOwned::to_owned)
}

/// Opens a URL in the session's browser, if there is one.
///
/// Detached and ignored: a panel menu must not block on a browser starting, and a machine with
/// no browser is not an error worth reporting from an About box.
pub fn open(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_component_that_answers_shows_its_own_version() {
        let c = components(Some("0.1.30".into()), Some("0.1.33".into()));
        assert_eq!(line(&c[1]), "argond  0.1.30");
        assert_eq!(line(&c[2]), "argonctl  0.1.33");
        // The tray reports itself, which is the one version it can know first-hand.
        assert_eq!(
            line(&c[0]),
            format!("argon-tray  {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn versions_are_not_assumed_to_match() {
        // A package upgrade leaves the running daemon on the old version until it restarts;
        // the menu must be able to show that rather than three copies of the tray's version.
        let c = components(Some("0.1.30".into()), Some("0.1.33".into()));
        assert_ne!(line(&c[1]), line(&c[2]));
    }

    #[test]
    fn a_silent_daemon_says_why_rather_than_unknown() {
        let c = components(None, None);
        assert_eq!(line(&c[1]), "argond  not answering on the bus");
        assert_eq!(line(&c[2]), "argonctl  not installed");
    }

    #[test]
    fn the_links_are_the_projects_own() {
        assert!(REPO_URL.starts_with("https://github.com/"));
        assert!(DONATE_URL.contains("paypal.com"));
    }
}
