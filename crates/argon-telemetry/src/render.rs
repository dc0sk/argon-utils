// SPDX-License-Identifier: GPL-3.0-or-later
//! Rendering a snapshot as Prometheus text exposition format.
//!
//! Pure: a struct in, a string out. That makes the output testable without opening a socket,
//! which matters because exposition format has rules that are easy to break silently — a
//! missing `# TYPE`, a label that is not escaped, a counter that goes backwards.

use std::fmt::Write as _;

/// Everything the exporter knows at one moment.
///
/// Every field is an `Option` where the underlying reading can genuinely be unavailable.
/// A absent metric is a scrape gap, which Prometheus handles; a fabricated zero is a lie that
/// looks exactly like data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Package version, for `argon_build_info`.
    pub version: &'static str,
    /// Operating mode name.
    pub mode: &'static str,
    /// CPU temperature in tenths of a degree Celsius.
    pub cpu_decicelsius: Option<i32>,
    /// Fan PWM as the kernel reports it, 0–255.
    pub fan_pwm: Option<u8>,
    /// Measured fan speed.
    pub fan_rpm: Option<u32>,
    /// Which fan backend is in use, if any.
    pub fan_backend: Option<&'static str>,
    /// Whether the fan backend can be written to.
    pub fan_writable: Option<bool>,
    /// Thermal governor cooling state and its maximum.
    pub governor_state: Option<(u32, u32)>,
    /// Vendor units and whether each is active.
    pub vendor_units: Vec<(String, bool)>,
    /// Devices discovered, and whether each is present.
    pub devices: Vec<(&'static str, bool)>,
    /// Consecutive temperature-read failures.
    pub sensor_failures: u32,
}

/// Renders a snapshot in Prometheus text exposition format.
#[must_use]
pub fn render(s: &Snapshot) -> String {
    let mut out = String::with_capacity(2048);
    render_identity(&mut out, s);
    render_thermal(&mut out, s);
    render_fan(&mut out, s);
    render_inventory(&mut out, s);
    out
}

/// Build and mode information.
fn render_identity(out: &mut String, s: &Snapshot) {
    metric(
        out,
        "argon_build_info",
        "gauge",
        "Build information. Always 1; the version is in the label.",
    );
    let _ = writeln!(
        out,
        "argon_build_info{{version=\"{}\"}} 1",
        escape(s.version)
    );

    metric(
        out,
        "argon_mode_info",
        "gauge",
        "The daemon's operating mode.",
    );
    let _ = writeln!(out, "argon_mode_info{{mode=\"{}\"}} 1", escape(s.mode));
}

/// Temperature and the thermal governor.
fn render_thermal(out: &mut String, s: &Snapshot) {
    if let Some(dc) = s.cpu_decicelsius {
        metric(
            out,
            "argon_cpu_temperature_celsius",
            "gauge",
            "CPU temperature from the kernel thermal zone.",
        );
        // Tenths to degrees, formatted rather than divided as a float: the reading has one
        // decimal place of real precision and printing more would imply precision we do not
        // have.
        let _ = writeln!(
            out,
            "argon_cpu_temperature_celsius {}.{}",
            dc / 10,
            (dc % 10).abs()
        );
    }

    if let Some((state, max)) = s.governor_state {
        metric(
            out,
            "argon_thermal_governor_state",
            "gauge",
            "Cooling state the kernel thermal governor is calling for.",
        );
        let _ = writeln!(out, "argon_thermal_governor_state {state}");
        metric(
            out,
            "argon_thermal_governor_max_state",
            "gauge",
            "Highest cooling state the governor can call for.",
        );
        let _ = writeln!(out, "argon_thermal_governor_max_state {max}");
    }

    metric(
        out,
        "argon_sensor_read_failures",
        "gauge",
        "Consecutive failed temperature reads. Non-zero means the fan is on its fallback.",
    );
    let _ = writeln!(out, "argon_sensor_read_failures {}", s.sensor_failures);
}

/// Fan speed and which backend drives it.
fn render_fan(out: &mut String, s: &Snapshot) {
    if let Some(pwm) = s.fan_pwm {
        metric(
            out,
            "argon_fan_pwm_ratio",
            "gauge",
            "Fan PWM duty as a ratio of full scale.",
        );
        // A ratio rather than the raw 0-255, per Prometheus convention: dashboards should not
        // have to know the kernel's scale.
        let _ = writeln!(out, "argon_fan_pwm_ratio {:.3}", f64::from(pwm) / 255.0);
    }

    if let Some(rpm) = s.fan_rpm {
        metric(out, "argon_fan_speed_rpm", "gauge", "Measured fan speed.");
        let _ = writeln!(out, "argon_fan_speed_rpm {rpm}");
    }

    if let Some(backend) = s.fan_backend {
        metric(
            out,
            "argon_fan_backend_info",
            "gauge",
            "Which backend drives the fan, and whether argon-utils may write to it.",
        );
        let writable = u8::from(s.fan_writable.unwrap_or(false));
        let _ = writeln!(
            out,
            "argon_fan_backend_info{{backend=\"{}\",writable=\"{}\"}} 1",
            escape(backend),
            writable
        );
    }
}

/// What else is on the machine, and what else is driving it.
fn render_inventory(out: &mut String, s: &Snapshot) {
    if !s.vendor_units.is_empty() {
        metric(
            out,
            "argon_vendor_daemon_active",
            "gauge",
            "Whether a vendor daemon is running and therefore contending for the hardware.",
        );
        for (unit, active) in &s.vendor_units {
            let _ = writeln!(
                out,
                "argon_vendor_daemon_active{{unit=\"{}\"}} {}",
                escape(unit),
                u8::from(*active)
            );
        }
    }

    if !s.devices.is_empty() {
        metric(
            out,
            "argon_device_present",
            "gauge",
            "Whether a device was discovered.",
        );
        for (device, present) in &s.devices {
            let _ = writeln!(
                out,
                "argon_device_present{{device=\"{}\"}} {}",
                escape(device),
                u8::from(*present)
            );
        }
    }
}

/// Writes the `# HELP` and `# TYPE` lines for a metric.
fn metric(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// Escapes a label value per the exposition format.
///
/// Backslash, double quote and newline must be escaped. An unescaped quote in a unit name
/// would produce a line that parses as something else entirely, which is the kind of bug that
/// only shows up on the one machine with an odd device name.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}
