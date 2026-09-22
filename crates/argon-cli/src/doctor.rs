// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl doctor` — read-only machine inspection.

use argon_hal::{discovery, fan_hwmon, foreign, platform};
use std::path::Path;
use std::process::ExitCode;

/// Lines on the RP1 that Argon hardware uses, and what for.
const LINES_OF_INTEREST: &[(u32, &str)] = &[
    (4, "power button pulses from the MCU"),
    (17, "power button (MCU side)"),
    (22, "IR transmit"),
    (23, "IR receive"),
    (27, "lid switch (ONE UP only)"),
];

/// Vendor configuration files worth reporting, since we will want to import them.
const VENDOR_CONFIGS: &[(&str, &str)] = &[
    ("/etc/argononed.conf", "CPU fan curve"),
    ("/etc/argononed-hdd.conf", "HDD fan curve (EON)"),
    ("/etc/argoneonoled.conf", "OLED display"),
    ("/etc/argonunits.conf", "temperature units"),
    ("/etc/argonupsrtc.conf", "UPS RTC schedules"),
    ("/etc/argonupsbattery.conf", "UPS battery meter"),
];

#[derive(clap::Args)]
pub struct Args {
    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: &Args) -> ExitCode {
    let plat = match platform::Platform::detect() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argonctl: cannot identify this machine: {e}");
            return ExitCode::FAILURE;
        }
    };

    let buses = discovery::i2c_buses().unwrap_or_default();
    let chips = discovery::gpio_chips();
    let usb = discovery::usb_devices();
    let units = foreign::vendor_units();

    if args.json {
        print_json(&plat, &buses, &chips, &usb, &units);
        return ExitCode::SUCCESS;
    }

    let mut warnings: Vec<String> = Vec::new();
    report_platform(&plat, &mut warnings);
    report_i2c(&buses, &mut warnings);
    report_gpio(&chips, &mut warnings);
    report_fan(&mut warnings);
    let ups = report_usb(&usb);
    let contended = report_contention(&units, ups);
    report_vendor_config();
    report_summary(contended, &warnings);

    ExitCode::SUCCESS
}

fn report_platform(plat: &platform::Platform, warnings: &mut Vec<String>) {
    section("Platform");
    kv("model", &plat.model);
    kv("revision", plat.revision.as_deref().unwrap_or("unknown"));
    kv("generation", &format!("{:?}", plat.generation));
    kv("kernel", &plat.kernel);
    kv("os", &format!("{} ({})", plat.os, plat.arch));

    if plat.generation == platform::PiGeneration::NotAPi {
        warnings.push("this does not look like a Raspberry Pi; nothing here will apply".into());
    }
}

fn report_i2c(buses: &[discovery::I2cBus], warnings: &mut Vec<String>) {
    section("I2C buses");
    if buses.is_empty() {
        println!("  none — is `dtparam=i2c_arm=on` set in config.txt?");
        warnings.push("no I2C buses found; fan and OLED control are impossible".into());
    }
    for b in buses {
        println!("  {} — {}", b.dev.display(), b.name);
        for (addr, driver) in &b.bound {
            println!("      0x{addr:02x} claimed by kernel driver `{driver}` (will not be probed)");
        }
    }
    println!(
        "\n  Not probed. A register read would put the register number on the bus as a write\n  \
         first, which legacy MCU firmware reads as a fan duty. See docs/design/adr/0002."
    );
}

fn report_gpio(chips: &[discovery::GpioChip], warnings: &mut Vec<String>) {
    section("GPIO");
    if chips.is_empty() {
        println!("  no gpiochips found");
    }
    for c in chips {
        println!(
            "  {} — label `{}`, {} lines",
            c.path.display(),
            c.label,
            c.lines
        );
    }

    // Resolved by the name the kernel gives a header line, not by the chip number -- which is
    // not stable, and /dev/gpiochip4 is a distro udev symlink rather than a kernel name -- and
    // not by the label either, which differs per board (rp1, bcm2711, bcm2835).
    let Some(header) = discovery::header_gpio_chip(crate::button::BUTTON_LINE) else {
        warnings.push("could not identify the header GPIO chip".into());
        return;
    };
    let header = &header;

    println!("\n  Header lines on `{}`:", header.label);
    let offsets: Vec<u32> = LINES_OF_INTEREST.iter().map(|(o, _)| *o).collect();
    let lines = discovery::gpio_lines(&header.path, &offsets);
    for line in &lines {
        let purpose = LINES_OF_INTEREST
            .iter()
            .find(|(o, _)| *o == line.offset)
            .map_or("", |(_, p)| *p);
        let held = line
            .consumer
            .as_deref()
            .map_or_else(|| "unheld".to_owned(), |c| format!("held by `{c}`"));
        println!(
            "    line {:>2} {:<8} {:<6} {:<22} {}",
            line.offset,
            line.name,
            if line.is_output { "out" } else { "in" },
            held,
            purpose
        );
    }

    // Which power button is this? On a Pi 5 the dedicated button is a kernel input device, and
    // an Argon ONE V5's case button is a mechanical extension of it: presses arrive as
    // KEY_POWER and the desktop or logind handles them. GPIO 4 only matters on Pi 4-era cases,
    // where an Argon MCU signals presses as pulses on that line.
    if let Some(dev) = discovery::pi_power_button() {
        println!(
            "\n  Power button: the Raspberry Pi's own button ({}), handled by the OS.",
            dev.display()
        );
        println!("  No Argon MCU is involved, so line 4 being unheld is expected here.");
        println!(
            "  Careful: on the Pi desktop a second press while the shutdown dialog is open runs\n  \
             `shutdown -h now`, and a logind inhibitor does not prevent it."
        );
    } else if lines.iter().any(|l| l.offset == 4 && l.consumer.is_none()) {
        warnings.push(
            "line 4 has no consumer. On Pi 4-era Argon cases that is where the MCU signals \
             button presses, so nothing would be handling them"
                .into(),
        );
    }
}

/// Reports who is actually driving the fan.
///
/// On an Argon ONE V5 with a Pi 5 the answer is the kernel, not an Argon MCU -- there is no
/// device at 0x1a at all. Saying so here saves the next person the afternoon it cost to find
/// out. See docs/protocol/captures/OBS-2026-09-16-v5-fan-is-kernel-controlled.md.
fn report_fan(warnings: &mut Vec<String>) {
    section("Fan");

    let Some(fan) = fan_hwmon::PwmFan::find() else {
        println!("  no kernel PWM fan found");
        println!("  If this case has a fan, it is driven some other way -- on Pi 4-era cases");
        println!("  that is the Argon MCU over I2C.");
        return;
    };

    println!("  kernel PWM fan at {}", fan.path().display());
    match fan.read() {
        Some(r) => {
            let rpm = r
                .rpm
                .map_or_else(|| "no tachometer".to_owned(), |v| format!("{v} rpm"));
            println!("  pwm {} / 255, {rpm}", r.pwm);
            if r.is_spinning() {
                println!("  the fan is currently turning");
            }
        }
        None => println!("  (could not read its state)"),
    }

    let cooling: Vec<_> = fan_hwmon::cooling_devices()
        .into_iter()
        .filter(|c| c.kind.contains("fan"))
        .collect();
    for c in &cooling {
        println!(
            "  thermal governor: {} at state {}/{}",
            c.kind, c.state, c.max_state
        );
    }

    if !cooling.is_empty() {
        warnings.push(
            "the kernel thermal governor is driving this fan. argon-utils does not control \
             it, and nothing needs to be at I2C 0x1a for the fan to work"
                .into(),
        );
    }
}

fn report_usb(usb: &[discovery::UsbDevice]) -> Option<&discovery::UsbDevice> {
    section("USB devices");
    let ups = discovery::find_argon_ups(usb);
    let zigbee = discovery::find_argon_zigbee(usb);

    for d in usb {
        let role = if Some(d.kernel.as_str()) == ups.map(|u| u.kernel.as_str()) {
            "  <- Argon PWR UPS"
        } else if Some(d.kernel.as_str()) == zigbee.map(|z| z.kernel.as_str()) {
            "  <- Argon Industria Zigbee (CC2652P)"
        } else {
            ""
        };
        let loc = d.controller.as_deref().map_or("?", |c| {
            if c == discovery::INTERNAL_USB_CONTROLLER {
                "internal"
            } else {
                "external"
            }
        });
        println!(
            "  {:<8} {}:{}  {:<8} {}{}",
            d.kernel,
            d.vid,
            d.pid,
            loc,
            d.product.as_deref().unwrap_or("-"),
            role
        );
        for n in &d.nodes {
            println!("      {}", n.display());
        }
    }

    if ups.is_none() {
        println!("\n  No Argon UPS found.");
    }
    if zigbee.is_none() && !usb.is_empty() {
        println!("  No Argon Zigbee module found on the internal header.");
    }
    ups
}

fn report_contention(units: &[foreign::UnitState], ups: Option<&discovery::UsbDevice>) -> bool {
    section("Other software using this hardware");
    let mut contended = false;
    for u in units {
        let marker = if u.is_active() { "ACTIVE" } else { "      " };
        println!("  {marker}  {:<26} {:<14} {}", u.unit, u.state, u.owns);
        contended |= u.is_active();
    }

    let Some(ups) = ups else { return contended };
    for node in ups.nodes.iter().filter(|n| is_tty(n)) {
        let owners = foreign::port_owners(node);
        if owners.is_empty() {
            let qualifier = if foreign::can_see_all_processes() {
                "free"
            } else {
                "no owner visible (not privileged — inconclusive)"
            };
            println!("\n  {} — {qualifier}", node.display());
        } else {
            contended = true;
            for o in owners {
                println!(
                    "\n  {} — held by pid {} ({})",
                    node.display(),
                    o.pid,
                    o.comm
                );
            }
        }
    }
    contended
}

fn report_vendor_config() {
    section("Vendor configuration present");
    let mut found_any = false;
    for (path, what) in VENDOR_CONFIGS {
        if Path::new(path).is_file() {
            found_any = true;
            println!("  {path:<32} {what}");
        }
    }
    if !found_any {
        println!("  none");
    }
}

fn report_summary(contended: bool, warnings: &[String]) {
    section("Summary");
    if contended {
        println!(
            "  The vendor software is currently driving this hardware. That is expected and\n  \
             fine — this report changed nothing. argon-utils will refuse to take control\n  \
             until you run an explicit takeover."
        );
    }
    for w in warnings {
        println!("  warning: {w}");
    }
    if warnings.is_empty() && !contended {
        println!("  Nothing contending for the hardware.");
    }
}

fn is_tty(p: &Path) -> bool {
    p.file_name().is_some_and(|n| {
        let n = n.to_string_lossy();
        n.starts_with("ttyUSB") || n.starts_with("ttyACM")
    })
}

fn section(title: &str) {
    println!("\n{title}");
    println!("{}", "-".repeat(title.len()));
}

fn kv(k: &str, v: &str) {
    println!("  {k:<12} {v}");
}

/// Minimal hand-rolled JSON. Deliberately dependency-free for now: the shape of this report
/// is still moving, and a serde derive would imply a stability we cannot yet promise.
fn print_json(
    plat: &platform::Platform,
    buses: &[discovery::I2cBus],
    chips: &[discovery::GpioChip],
    usb: &[discovery::UsbDevice],
    units: &[foreign::UnitState],
) {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }
    println!("{{");
    println!("  \"platform\": {{");
    println!("    \"model\": \"{}\",", esc(&plat.model));
    println!(
        "    \"revision\": \"{}\",",
        esc(plat.revision.as_deref().unwrap_or(""))
    );
    println!("    \"generation\": \"{:?}\",", plat.generation);
    println!("    \"kernel\": \"{}\",", esc(&plat.kernel));
    println!("    \"os\": \"{}\",", esc(&plat.os));
    println!("    \"arch\": \"{}\"", esc(&plat.arch));
    println!("  }},");

    println!("  \"i2c\": [");
    for (i, b) in buses.iter().enumerate() {
        let comma = if i + 1 == buses.len() { "" } else { "," };
        println!(
            "    {{ \"dev\": \"{}\", \"name\": \"{}\" }}{comma}",
            b.dev.display(),
            esc(&b.name)
        );
    }
    println!("  ],");

    println!("  \"gpiochips\": [");
    for (i, c) in chips.iter().enumerate() {
        let comma = if i + 1 == chips.len() { "" } else { "," };
        println!(
            "    {{ \"path\": \"{}\", \"label\": \"{}\", \"lines\": {} }}{comma}",
            c.path.display(),
            esc(&c.label),
            c.lines
        );
    }
    println!("  ],");

    println!("  \"usb\": [");
    for (i, d) in usb.iter().enumerate() {
        let comma = if i + 1 == usb.len() { "" } else { "," };
        let nodes: Vec<String> = d
            .nodes
            .iter()
            .map(|n| format!("\"{}\"", n.display()))
            .collect();
        println!(
            "    {{ \"kernel\": \"{}\", \"vid\": \"{}\", \"pid\": \"{}\", \"product\": \"{}\", \
             \"serial\": \"{}\", \"controller\": \"{}\", \"nodes\": [{}] }}{comma}",
            esc(&d.kernel),
            esc(&d.vid),
            esc(&d.pid),
            esc(d.product.as_deref().unwrap_or("")),
            esc(d.serial.as_deref().unwrap_or("")),
            esc(d.controller.as_deref().unwrap_or("")),
            nodes.join(", ")
        );
    }
    println!("  ],");

    println!("  \"vendor_units\": [");
    for (i, u) in units.iter().enumerate() {
        let comma = if i + 1 == units.len() { "" } else { "," };
        println!(
            "    {{ \"unit\": \"{}\", \"state\": \"{}\", \"active\": {} }}{comma}",
            esc(&u.unit),
            esc(&u.state),
            u.is_active()
        );
    }
    println!("  ]");
    println!("}}");
}
