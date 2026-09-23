// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl setup` — is this machine configured for the Argon hardware it has?
//!
//! `doctor` inventories what is present. This answers the next question: what is **missing**,
//! and exactly which command fixes it. The two are separate because every finding here has a
//! remedy, and a remedy is only useful if it is the precise line to run.
//!
//! Read-only: it reads `config.txt`, asks the package manager and systemd, and probes I2C
//! addresses only with the `SMBus` quick-write that carries no data byte (ADR-0002). It writes
//! nothing, and prints the commands for a person to run instead -- boot configuration needs
//! root and a reboot, and neither belongs inside a status command.
//!
//! What is "required" depends on the case: a ONE V5 needs the internal USB hub, a ONE UP needs
//! its lid overlay, a Pi 4-era case needs the header I2C bus and, for IR, the `gpio-ir` overlay.
//! So the checks are derived from what the machine actually has, not from a fixed list.

use argon_device::boot_config;
use argon_hal::i2c::I2cBus as _;
use argon_hal::{discovery, foreign, platform};
use std::process::ExitCode;

#[derive(clap::Args)]
pub struct Args {
    /// Machine-readable output: one JSON object.
    #[arg(long)]
    pub json: bool,
}

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Configured, present, or otherwise fine.
    Ok,
    /// Needed here, and not configured.
    Missing,
    /// Optional, not configured. Not a fault.
    Optional,
    /// Does not apply to this machine.
    NotApplicable,
}

impl State {
    /// Fixed width, so the columns line up whatever the mix of states.
    const fn mark(self) -> &'static str {
        match self {
            Self::Ok => "ok     ",
            Self::Missing => "MISSING",
            Self::Optional => "--     ",
            Self::NotApplicable => "n/a    ",
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Optional => "optional",
            Self::NotApplicable => "not-applicable",
        }
    }
}

/// One thing that was checked.
#[derive(Debug, Clone)]
pub struct Check {
    /// What was checked, in a few words.
    pub name: String,
    /// How it came out.
    pub state: State,
    /// What was found, said plainly.
    pub detail: String,
    /// The exact command that fixes it, when something needs fixing.
    pub fix: Option<String>,
}

impl Check {
    fn new(name: &str, state: State, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            state,
            detail: detail.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// What the machine looks like, as plain data, so the reasoning can be tested without one.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a bag of independent observations about one machine, not a state machine"
)]
pub struct Facts {
    /// Device-tree model string.
    pub model: String,
    /// Contents of `config.txt`, if it could be read.
    pub config_txt: Option<String>,
    /// Where that file is.
    pub config_path: String,
    /// Whether the 40-pin header I2C bus exists.
    pub header_bus: bool,
    /// Whether something acknowledges the MCU address. `None`: not attempted.
    pub mcu_answers: Option<bool>,
    /// Whether something acknowledges the OLED address.
    pub oled_answers: Option<bool>,
    /// Whether something acknowledges the ONE UP fuel gauge's address.
    pub gauge_answers: Option<bool>,
    /// Whether the PWR UPS is on USB.
    pub ups_present: bool,
    /// Whether an IR receiver device exists now.
    pub lirc: bool,
    /// The login user's groups.
    pub groups: Vec<String>,
    /// Our package's version, if installed.
    pub package: Option<String>,
    /// Whether `ir-ctl` is available.
    pub ir_ctl: bool,
    /// Whether the packaged udev rules are installed.
    pub udev_rules: bool,
    /// Whether `argond` is running.
    pub argond_active: bool,
    /// Vendor units that are present, with whether each is active.
    pub vendor_units: Vec<(String, bool)>,
    /// Whether the vendor's files are unpacked in `/etc/argon`.
    pub vendor_files: bool,
}

impl Facts {
    /// The `config.txt` filter sections in force on this board.
    fn sections(&self) -> Vec<&'static str> {
        boot_config::sections_for(&self.model)
    }

    /// Whether this looks like an Argon ONE UP: the CM5 laptop, identified by its fuel gauge.
    fn is_one_up(&self) -> bool {
        self.gauge_answers == Some(true)
            || self.model.to_ascii_lowercase().contains("compute module 5")
    }

    /// Whether a Pi 4-era case with a microcontroller is present.
    fn has_mcu(&self) -> bool {
        self.mcu_answers == Some(true)
    }

    /// Whether the board is a Pi 5, where the case button and fan are the Pi's own.
    fn is_pi5(&self) -> bool {
        let m = self.model.to_ascii_lowercase();
        m.contains("pi 5") || m.contains("compute module 5")
    }
}

/// A named group of checks.
#[derive(Debug, Clone)]
pub struct Section {
    /// Heading.
    pub name: &'static str,
    /// What was checked under it.
    pub checks: Vec<Check>,
}

/// Works out what this machine needs and what it is missing.
///
/// Pure: every input is in [`Facts`], so the rules are testable against a machine that is not
/// in the room.
#[must_use]
pub fn review(f: &Facts) -> Vec<Section> {
    vec![
        Section {
            name: "Boot configuration",
            checks: boot_checks(f),
        },
        Section {
            name: "Software",
            checks: software_checks(f),
        },
        Section {
            name: "Access",
            checks: access_checks(f),
        },
        Section {
            name: "Services",
            checks: service_checks(f),
        },
    ]
}

fn boot_checks(f: &Facts) -> Vec<Check> {
    let sections = f.sections();
    let Some(text) = &f.config_txt else {
        return vec![Check::new(
            "config.txt",
            State::Missing,
            format!("cannot read {}", f.config_path),
        )];
    };
    let mut out = Vec::new();

    // The header I2C bus: every Argon device but the UPS sits on it.
    let i2c_needed = f.has_mcu() || f.is_one_up() || f.oled_answers == Some(true);
    let i2c_set = boot_config::is_active(text, "dtparam", "i2c_arm=on", &sections);
    out.push(match (f.header_bus, i2c_set) {
        (true, _) => Check::new("header I2C bus", State::Ok, "/dev/i2c-1 is present"),
        (false, true) => Check::new(
            "header I2C bus",
            State::Missing,
            "config.txt enables it, but no /dev/i2c-1 — a reboot is pending",
        )
        .with_fix("sudo reboot"),
        (false, false) if i2c_needed => Check::new(
            "header I2C bus",
            State::Missing,
            "no /dev/i2c-1, and dtparam=i2c_arm=on is not set",
        )
        .with_fix(format!(
            "echo 'dtparam=i2c_arm=on' | sudo tee -a {} && sudo reboot",
            f.config_path
        )),
        (false, false) => Check::new(
            "header I2C bus",
            State::Optional,
            "not enabled; nothing here needs it yet",
        )
        .with_fix(format!(
            "echo 'dtparam=i2c_arm=on' | sudo tee -a {}",
            f.config_path
        )),
    });

    // The internal USB hub carries the PWR UPS and the Zigbee module on a ONE V5.
    let dwc2 = boot_config::is_active_prefix(text, "dtoverlay", "dwc2", &sections);
    out.push(if f.ups_present {
        Check::new(
            "internal USB hub",
            State::Ok,
            "the UPS is enumerated, so the hub is live",
        )
    } else if dwc2 {
        Check::new("internal USB hub", State::Ok, "dtoverlay=dwc2 is set")
    } else if f.is_pi5() {
        Check::new(
            "internal USB hub",
            State::Optional,
            "dtoverlay=dwc2,dr_mode=host is not set; the case's internal header \
             (UPS, Zigbee) stays dark without it",
        )
        .with_fix(format!(
            "echo 'dtoverlay=dwc2,dr_mode=host' | sudo tee -a {}",
            f.config_path
        ))
    } else {
        Check::new("internal USB hub", State::NotApplicable, "not a Pi 5 case")
    });

    out.push(ir_check(f, text, &sections));
    out.push(lid_check(f, text, &sections));
    out
}

/// IR: a Pi 4-era case has a receiver on BCM 23. The V5 demonstrably has none (T6).
fn ir_check(f: &Facts, text: &str, sections: &[&str]) -> Check {
    // IR: a Pi 4-era case has a receiver on BCM 23. The V5 demonstrably has none.
    let ir_set = boot_config::is_active_prefix(text, "dtoverlay", "gpio-ir", sections);
    if ir_set && f.lirc {
        Check::new("IR receiver", State::Ok, "gpio-ir is configured and bound")
    } else if ir_set {
        Check::new(
            "IR receiver",
            State::Missing,
            "gpio-ir is in config.txt but no /dev/lirc0 — a reboot is pending",
        )
        .with_fix("sudo reboot")
    } else if f.lirc {
        Check::new(
            "IR receiver",
            State::Missing,
            "bound now, but not in config.txt: it will be gone after a reboot",
        )
        .with_fix(format!(
            "echo 'dtoverlay=gpio-ir,gpio_pin=23' | sudo tee -a {}",
            f.config_path
        ))
    } else if f.is_pi5() {
        // Careful with the claim: T6 measured the ONE V5, not every Pi 5 enclosure. Other
        // Pi 5 cases are unmeasured, and saying "the V5 has none" as though it settled them
        // would be inheriting an answer rather than having one.
        Check::new(
            "IR receiver",
            State::Optional,
            "not configured. The ONE V5 has no receiver wired (T6); other Pi 5 cases are              unmeasured, so this is worth trying only if your case has an IR window",
        )
        .with_fix(format!(
            "echo 'dtoverlay=gpio-ir,gpio_pin=23' | sudo tee -a {} && sudo reboot",
            f.config_path
        ))
    } else {
        Check::new(
            "IR receiver",
            State::Optional,
            "not configured; Pi 4-era cases have a receiver on BCM 23",
        )
        .with_fix(format!(
            "echo 'dtoverlay=gpio-ir,gpio_pin=23' | sudo tee -a {} && sudo reboot",
            f.config_path
        ))
    }
}

/// The ONE UP's lid, which is not a lid switch at all without our overlay.
fn lid_check(f: &Facts, text: &str, sections: &[&str]) -> Check {
    // The ONE UP's lid, which needs our overlay to become a lid switch at all.
    if !f.is_one_up() {
        Check::new("ONE UP lid switch", State::NotApplicable, "not a ONE UP")
    } else if boot_config::is_active_prefix(text, "dtoverlay", "argon-oneup-lid", sections) {
        Check::new(
            "ONE UP lid switch",
            State::Ok,
            "the lid overlay is configured",
        )
    } else {
        Check::new(
            "ONE UP lid switch",
            State::Optional,
            "the lid is not a lid switch without the overlay",
        )
        .with_fix("sudo /usr/libexec/argon-utils/oneup-takeover")
    }
}

fn software_checks(f: &Facts) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(match &f.package {
        Some(v) => Check::new("argon-utils package", State::Ok, format!("version {v}")),
        None => Check::new(
            "argon-utils package",
            State::Optional,
            "not installed; running from a build tree is fine for testing, but the daemon, \
             udev rules and polkit action come with the package",
        )
        .with_fix("dpkg-buildpackage -b -us -uc && sudo apt install ../argon-utils_*.deb"),
    });

    // ir-ctl is only worth having where IR exists.
    let ir_relevant = f.lirc || !f.is_pi5();
    out.push(match (f.ir_ctl, ir_relevant) {
        (true, _) => Check::new("ir-ctl (v4l-utils)", State::Ok, "available"),
        (false, true) => Check::new(
            "ir-ctl (v4l-utils)",
            State::Optional,
            "not installed; it is how raw IR is captured and decoded",
        )
        .with_fix("sudo apt install v4l-utils"),
        (false, false) => Check::new("ir-ctl (v4l-utils)", State::NotApplicable, "no IR here"),
    });

    out.push(if f.vendor_files {
        Check::new(
            "vendor software",
            State::Optional,
            "/etc/argon is present. argon-utils does not need it, and the package retires \
             the vendor's UPS daemons on install, recording what it disabled",
        )
    } else {
        Check::new("vendor software", State::Ok, "not installed")
    });

    out
}

fn access_checks(f: &Facts) -> Vec<Check> {
    let has = |g: &str| f.groups.iter().any(|x| x == g);
    let mut out = Vec::new();

    for (group, what, needed) in [
        (
            "i2c",
            "reading the MCU, OLED or fuel gauge by hand",
            f.header_bus,
        ),
        ("gpio", "watching the button and lid lines", true),
        ("video", "reading /dev/lirc0 for IR", f.lirc),
    ] {
        out.push(if has(group) {
            Check::new(&format!("group `{group}`"), State::Ok, "you are a member")
        } else if needed {
            Check::new(
                &format!("group `{group}`"),
                State::Optional,
                format!("not a member; needed for {what} without sudo"),
            )
            .with_fix(format!("sudo adduser $USER {group}   # then log in again"))
        } else {
            Check::new(
                &format!("group `{group}`"),
                State::NotApplicable,
                "not needed here",
            )
        });
    }

    // Deliberately not a recommendation: the argon group owns a link with no arbitration.
    out.push(Check::new(
        "group `argon`",
        State::Ok,
        "do not join it: it owns the UPS port and hidraw node so that exactly one process \
         does. `argonctl ups` asks the daemon over D-Bus instead",
    ));

    out.push(match (f.ups_present, f.udev_rules) {
        (_, true) => Check::new("udev rules", State::Ok, "installed"),
        (true, false) => Check::new(
            "udev rules",
            State::Missing,
            "a UPS is present but the rules are not installed, so its nodes stay root-only",
        )
        .with_fix("sudo apt install ../argon-utils_*.deb"),
        (false, false) => Check::new("udev rules", State::NotApplicable, "no UPS present"),
    });

    out
}

fn service_checks(f: &Facts) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(if f.argond_active {
        Check::new("argond", State::Ok, "running")
    } else if f.package.is_some() {
        Check::new("argond", State::Missing, "installed but not running")
            .with_fix("sudo systemctl start argond && journalctl -u argond -n 20 --no-pager")
    } else {
        Check::new(
            "argond",
            State::NotApplicable,
            "the package is not installed",
        )
    });

    let contending: Vec<&str> = f
        .vendor_units
        .iter()
        .filter(|(_, active)| *active)
        .map(|(u, _)| u.as_str())
        .collect();
    out.push(if contending.is_empty() {
        Check::new("vendor daemons", State::Ok, "none active")
    } else {
        Check::new(
            "vendor daemons",
            State::Missing,
            format!(
                "active: {}. Two readers on the UPS serial port corrupt each other's frames",
                contending.join(", ")
            ),
        )
        .with_fix(format!("sudo systemctl stop {}", contending.join(" ")))
    });

    out
}

/// Runs the checks against this machine and prints them.
pub fn run(args: &Args) -> ExitCode {
    let facts = gather();
    let sections = review(&facts);
    if args.json {
        println!("{}", json(&facts, &sections));
        return ExitCode::SUCCESS;
    }

    println!("Setup check for {}\n", facts.model);
    let mut missing = 0;
    for section in &sections {
        println!("{}", section.name);
        println!("{}", "-".repeat(section.name.len()));
        for c in &section.checks {
            if c.state == State::Missing {
                missing += 1;
            }
            println!("  [{}] {:<22} {}", c.state.mark(), c.name, c.detail);
            if let Some(fix) = &c.fix {
                println!("       -> {fix}");
            }
        }
        println!();
    }

    if missing == 0 {
        println!("Nothing missing. Lines marked -- are optional extras.");
    } else {
        println!("{missing} thing(s) need attention, with the command beside each.");
    }
    ExitCode::SUCCESS
}

fn json(facts: &Facts, sections: &[Section]) -> String {
    let secs: Vec<_> = sections
        .iter()
        .map(|s| {
            let checks: Vec<_> = s
                .checks
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "state": c.state.name(),
                        "detail": c.detail,
                        "fix": c.fix,
                    })
                })
                .collect();
            serde_json::json!({ "section": s.name, "checks": checks })
        })
        .collect();
    serde_json::json!({
        "model": facts.model,
        "missing": sections
            .iter()
            .flat_map(|s| &s.checks)
            .filter(|c| c.state == State::Missing)
            .count(),
        "sections": secs,
    })
    .to_string()
}

/// Reads the machine. Probes I2C addresses only with the quick-write that carries no data.
fn gather() -> Facts {
    let plat = platform::Platform::detect().ok();
    let config_path = "/boot/firmware/config.txt";
    let buses = discovery::i2c_buses().unwrap_or_default();
    let header = buses.iter().any(|b| b.dev.ends_with("i2c-1"));
    let probe = |addr: u8| -> Option<bool> {
        if !header {
            return None;
        }
        argon_hal::i2c::LinuxI2c::open("/dev/i2c-1", u16::from(addr))
            .ok()
            .and_then(|b| argon_hal::i2c::ReadOnly(b).probe(addr).ok())
    };

    Facts {
        model: plat
            .as_ref()
            .map_or_else(|| "unknown".into(), |p| p.model.clone()),
        config_txt: std::fs::read_to_string(config_path).ok(),
        config_path: config_path.to_owned(),
        header_bus: header,
        mcu_answers: probe(argon_device::mcu::ADDR),
        oled_answers: probe(0x3c),
        gauge_answers: probe(argon_proto::cw2217::ADDR),
        ups_present: discovery::argon_ups_serial_path().is_some(),
        lirc: std::path::Path::new("/dev/lirc0").exists(),
        groups: groups(),
        package: package_version(),
        ir_ctl: which("ir-ctl"),
        udev_rules: std::path::Path::new("/lib/udev/rules.d/60-argon-utils.rules").exists()
            || std::path::Path::new("/etc/udev/rules.d/60-argon-utils.rules").exists(),
        argond_active: unit_active("argond.service"),
        vendor_units: foreign::vendor_units()
            .into_iter()
            .map(|u| (u.unit.clone(), u.is_active()))
            .collect(),
        vendor_files: std::path::Path::new("/etc/argon").exists(),
    }
}

fn groups() -> Vec<String> {
    std::process::Command::new("id")
        .arg("-nG")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.split_whitespace().map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

fn package_version() -> Option<String> {
    let out = std::process::Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", "argon-utils"])
        .output()
        .ok()?;
    let v = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn which(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null")])
        .status()
        .is_ok_and(|s| s.success())
}

fn unit_active(unit: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::{Facts, State, review};

    fn check<'a>(sections: &'a [super::Section], name: &str) -> &'a super::Check {
        sections
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no check named {name}"))
    }

    fn pi4_with_mcu() -> Facts {
        Facts {
            model: "Raspberry Pi 4 Model B Rev 1.4".into(),
            config_txt: Some(
                "dtparam=audio=on\n#dtparam=i2c_arm=on\n[all]\nenable_uart=1\n".into(),
            ),
            config_path: "/boot/firmware/config.txt".into(),
            header_bus: false,
            mcu_answers: None,
            groups: vec!["gpio".into()],
            ..Facts::default()
        }
    }

    #[test]
    fn the_commented_out_i2c_line_is_not_mistaken_for_configuration() {
        // Exactly the Pi 4 case: the stock config.txt ships the line commented, so there is
        // no /dev/i2c-1 and nothing can reach the MCU. A grep would have called this done.
        let mut f = pi4_with_mcu();
        f.oled_answers = Some(true); // something on the header bus is wanted
        let r = review(&f);
        let c = check(&r, "header I2C bus");
        assert_eq!(c.state, State::Missing);
        assert!(
            c.fix.as_ref().is_some_and(|x| x.contains("i2c_arm=on")),
            "{c:?}"
        );
        assert!(c.fix.as_ref().is_some_and(|x| x.contains("reboot")));
    }

    #[test]
    fn a_pending_reboot_is_named_as_such_rather_than_as_a_missing_setting() {
        let mut f = pi4_with_mcu();
        f.config_txt = Some("[all]\ndtparam=i2c_arm=on\n".into());
        let r = review(&f);
        let c = check(&r, "header I2C bus");
        assert_eq!(c.state, State::Missing);
        assert!(c.detail.contains("reboot is pending"), "{}", c.detail);
        assert_eq!(c.fix.as_deref(), Some("sudo reboot"));
    }

    #[test]
    fn ir_bound_only_at_runtime_is_flagged_as_not_surviving_a_reboot() {
        // The state the Pi 4 was left in after T6: working now, gone after a reboot.
        let mut f = pi4_with_mcu();
        f.lirc = true;
        let r = review(&f);
        let c = check(&r, "IR receiver");
        assert_eq!(c.state, State::Missing);
        assert!(c.detail.contains("after a reboot"), "{}", c.detail);
        assert!(c.fix.as_ref().is_some_and(|x| x.contains("gpio-ir")));
    }

    #[test]
    fn ir_on_a_pi5_cites_the_v5_without_claiming_every_case() {
        // T6 measured the ONE V5. A NEO 5 is a different enclosure, and inheriting the V5's
        // answer for it would be a guess wearing a measurement's clothes.
        let f = Facts {
            model: "Raspberry Pi 5 Model B Rev 1.1".into(),
            config_txt: Some("[all]\n".into()),
            header_bus: true,
            ..Facts::default()
        };
        let r = review(&f);
        let c = check(&r, "IR receiver");
        assert_eq!(c.state, State::Optional);
        assert!(c.detail.contains("ONE V5"), "{}", c.detail);
        assert!(c.detail.contains("unmeasured"), "{}", c.detail);
    }

    #[test]
    fn a_one_up_without_the_lid_overlay_is_told_how_to_get_one() {
        let f = Facts {
            model: "Raspberry Pi Compute Module 5 Rev 1.0".into(),
            config_txt: Some("[all]\n".into()),
            header_bus: true,
            gauge_answers: Some(true),
            ..Facts::default()
        };
        let r = review(&f);
        let c = check(&r, "ONE UP lid switch");
        assert_eq!(c.state, State::Optional);
        assert!(c.fix.as_ref().is_some_and(|x| x.contains("oneup-takeover")));
    }

    #[test]
    fn an_active_vendor_daemon_is_a_conflict_with_the_command_to_stop_it() {
        let f = Facts {
            model: "Raspberry Pi 5 Model B Rev 1.1".into(),
            config_txt: Some("[all]\n".into()),
            vendor_units: vec![
                ("argonupsrtcd.service".into(), true),
                ("argononed.service".into(), false),
            ],
            ..Facts::default()
        };
        let r = review(&f);
        let c = check(&r, "vendor daemons");
        assert_eq!(c.state, State::Missing);
        assert!(c.detail.contains("argonupsrtcd.service"));
        assert!(
            !c.detail.contains("argononed.service"),
            "named an inactive unit"
        );
        assert_eq!(
            c.fix.as_deref(),
            Some("sudo systemctl stop argonupsrtcd.service")
        );
    }

    #[test]
    fn joining_the_argon_group_is_never_the_advice() {
        let f = Facts {
            model: "Raspberry Pi 5 Model B Rev 1.1".into(),
            config_txt: Some("[all]\n".into()),
            ups_present: true,
            ..Facts::default()
        };
        let r = review(&f);
        let c = check(&r, "group `argon`");
        assert_eq!(c.state, State::Ok);
        assert!(c.fix.is_none(), "offered a command to join the argon group");
        assert!(c.detail.contains("do not join"));
        // And no other check may suggest it either.
        for c in r.iter().flat_map(|s| &s.checks) {
            if let Some(fix) = &c.fix {
                assert!(!fix.contains("adduser $USER argon"), "{}: {fix}", c.name);
            }
        }
    }

    #[test]
    fn an_unreadable_config_txt_is_one_clear_failure_not_four_guesses() {
        let f = Facts {
            model: "Raspberry Pi 4 Model B Rev 1.4".into(),
            config_txt: None,
            config_path: "/boot/firmware/config.txt".into(),
            ..Facts::default()
        };
        let boot = &review(&f)[0];
        assert_eq!(boot.checks.len(), 1);
        assert_eq!(boot.checks[0].state, State::Missing);
    }
}
