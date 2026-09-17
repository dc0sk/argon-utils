// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only discovery of the buses and devices we care about.
//!
//! Nothing here opens a serial port. See the crate docs for why.

use crate::{Result, read_trimmed};
use std::path::{Path, PathBuf};

/// An I2C adapter.
#[derive(Debug, Clone)]
pub struct I2cBus {
    /// Bus number, i.e. the `N` in `/dev/i2c-N`.
    pub number: u32,
    /// Adapter name from sysfs, e.g. `Synopsys DesignWare I2C adapter`.
    ///
    /// This, not the number, is the stable way to identify the bus: numbering depends on
    /// probe order and on which overlays are loaded.
    pub name: String,
    /// Device node path.
    pub dev: PathBuf,
    /// Addresses on this bus already claimed by a kernel driver, with the driver's name.
    ///
    /// These must never be probed. On the developers' own machine `0x4d` is a `HiFiBerry`
    /// `pcm5122`; poking it would be pointless at best.
    pub bound: Vec<(u16, String)>,
}

/// Enumerates I2C adapters. Read-only.
pub fn i2c_buses() -> Result<Vec<I2cBus>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/i2c-dev") else {
        return Ok(out);
    };
    for e in entries.flatten() {
        let name_os = e.file_name();
        let Some(rest) = name_os
            .to_string_lossy()
            .strip_prefix("i2c-")
            .map(str::to_owned)
        else {
            continue;
        };
        let Ok(number) = rest.parse::<u32>() else {
            continue;
        };
        let name = read_trimmed(e.path().join("name")).unwrap_or_default();

        // Kernel-bound clients appear as `N-XXXX` directories under the adapter.
        let mut bound = Vec::new();
        let adapter_dir = PathBuf::from(format!("/sys/bus/i2c/devices/i2c-{number}"));
        if let Ok(clients) = std::fs::read_dir(&adapter_dir) {
            for c in clients.flatten() {
                let cname = c.file_name().to_string_lossy().into_owned();
                let Some((bus, addr)) = cname.split_once('-') else {
                    continue;
                };
                if bus.parse::<u32>() != Ok(number) {
                    continue;
                }
                let Ok(addr) = u16::from_str_radix(addr, 16) else {
                    continue;
                };
                let driver = std::fs::read_link(c.path().join("driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .or_else(|| read_trimmed(c.path().join("name")).ok())
                    .unwrap_or_else(|| "unknown".into());
                bound.push((addr, driver));
            }
        }
        bound.sort_unstable();

        out.push(I2cBus {
            number,
            name,
            dev: PathBuf::from(format!("/dev/i2c-{number}")),
            bound,
        });
    }
    out.sort_by_key(|b| b.number);
    Ok(out)
}

/// A GPIO chip and the lines we care about.
#[derive(Debug, Clone)]
pub struct GpioChip {
    /// Chip path, e.g. `/dev/gpiochip0`.
    pub path: PathBuf,
    /// Chip label, e.g. `pinctrl-rp1`. This is how we identify it; the number is not stable.
    pub label: String,
    /// Number of lines.
    pub lines: usize,
}

/// State of a single GPIO line.
#[derive(Debug, Clone)]
pub struct GpioLine {
    /// Line offset within the chip.
    pub offset: u32,
    /// Kernel-assigned name, e.g. `GPIO4`.
    pub name: String,
    /// The process or driver currently holding the line, if any.
    pub consumer: Option<String>,
    /// Whether the line is configured as an output.
    pub is_output: bool,
}

/// Enumerates GPIO chips. Read-only — requests no lines.
#[must_use]
pub fn gpio_chips() -> Vec<GpioChip> {
    let mut out = Vec::new();
    for path in gpiocdev::chip::chips().into_iter().flatten() {
        if let Ok(c) = gpiocdev::chip::Chip::from_path(&path) {
            if let Ok(info) = c.info() {
                out.push(GpioChip {
                    path,
                    label: info.label.clone(),
                    lines: info.num_lines as usize,
                });
            }
        }
    }
    out
}

/// Reads the state of the given line offsets on a chip, without requesting them.
#[must_use]
pub fn gpio_lines(chip: &Path, offsets: &[u32]) -> Vec<GpioLine> {
    let Ok(c) = gpiocdev::chip::Chip::from_path(chip) else {
        return Vec::new();
    };
    offsets
        .iter()
        .filter_map(|&offset| {
            let info = c.line_info(offset).ok()?;
            Some(GpioLine {
                offset,
                name: info.name.clone(),
                consumer: if info.consumer.is_empty() {
                    None
                } else {
                    Some(info.consumer.clone())
                },
                is_output: info.direction == gpiocdev::line::Direction::Output,
            })
        })
        .collect()
}

/// A USB device we recognise, described entirely from sysfs.
#[derive(Debug, Clone)]
pub struct UsbDevice {
    /// Sysfs kernel name, e.g. `1-1.2`.
    pub kernel: String,
    /// Vendor ID.
    pub vid: String,
    /// Product ID.
    pub pid: String,
    /// iManufacturer string, if present.
    pub manufacturer: Option<String>,
    /// iProduct string, if present.
    pub product: Option<String>,
    /// iSerial string, if present.
    pub serial: Option<String>,
    /// The platform USB controller this device hangs off, e.g. `1000480000.usb`.
    ///
    /// This is the discriminator that separates the case's *internal* header from the
    /// external USB-A ports. VID:PID alone cannot do it: the Argon Zigbee module is a
    /// CP2102N, and so are common external USB-serial adapters.
    pub controller: Option<String>,
    /// Character device nodes this device provides, e.g. `/dev/ttyUSB0`, `/dev/hidraw0`.
    pub nodes: Vec<PathBuf>,
}

impl UsbDevice {
    /// Whether this device is on the given platform USB controller.
    #[must_use]
    pub fn on_controller(&self, controller: &str) -> bool {
        self.controller.as_deref() == Some(controller)
    }
}

/// Walks up from a sysfs path to the USB device directory that owns it.
fn usb_parent(mut p: PathBuf) -> Option<PathBuf> {
    loop {
        if p.join("idVendor").is_file() {
            return Some(p);
        }
        if !p.pop() || p == Path::new("/") {
            return None;
        }
    }
}

/// Extracts the platform USB controller name from a resolved sysfs path.
fn controller_of(p: &Path) -> Option<String> {
    // e.g. /sys/devices/platform/axi/1000480000.usb/usb1/1-1/1-1.2/...
    // Not a filename extension: platform device nodes are named `<addr>.<type>`, so the
    // `.usb` suffix identifies a USB host controller node in the device tree.
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .find(|s| {
            std::path::Path::new(s)
                .extension()
                .is_some_and(|e| e == "usb")
        })
}

/// Enumerates USB devices that expose a tty or hidraw node. Read-only; opens nothing.
#[must_use]
pub fn usb_devices() -> Vec<UsbDevice> {
    let mut by_kernel: std::collections::BTreeMap<String, UsbDevice> =
        std::collections::BTreeMap::new();

    for (class, prefixes) in [
        ("tty", &["ttyUSB", "ttyACM"][..]),
        ("hidraw", &["hidraw"][..]),
    ] {
        let Ok(entries) = std::fs::read_dir(format!("/sys/class/{class}")) else {
            continue;
        };
        for e in entries.flatten() {
            let node_name = e.file_name().to_string_lossy().into_owned();
            if !prefixes.iter().any(|p| node_name.starts_with(p)) {
                continue;
            }
            let Ok(real) = std::fs::canonicalize(e.path()) else {
                continue;
            };
            let Some(usb_dir) = usb_parent(real.clone()) else {
                continue;
            };
            let kernel = usb_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let entry = by_kernel
                .entry(kernel.clone())
                .or_insert_with(|| UsbDevice {
                    kernel,
                    vid: read_trimmed(usb_dir.join("idVendor")).unwrap_or_default(),
                    pid: read_trimmed(usb_dir.join("idProduct")).unwrap_or_default(),
                    manufacturer: read_trimmed(usb_dir.join("manufacturer")).ok(),
                    product: read_trimmed(usb_dir.join("product")).ok(),
                    serial: read_trimmed(usb_dir.join("serial")).ok(),
                    controller: controller_of(&usb_dir),
                    nodes: Vec::new(),
                });
            entry.nodes.push(PathBuf::from(format!("/dev/{node_name}")));
        }
    }

    let mut out: Vec<_> = by_kernel.into_values().collect();
    for d in &mut out {
        d.nodes.sort();
    }
    out
}

/// The platform USB controller behind the Argon ONE V5's internal 6-pin header.
///
/// The header is a `dwc2` host controller, live only when `dtoverlay=dwc2,dr_mode=host` is
/// set. Both the PWR UPS and the Industria Zigbee module hang off a hub on it.
pub const INTERNAL_USB_CONTROLLER: &str = "1000480000.usb";

/// Identifies the Argon PWR UPS among discovered USB devices.
///
/// Matched on **string descriptors**, never on VID:PID: the UPS enumerates as `1d6b:0104`,
/// the generic Linux USB gadget identifiers, which any number of unrelated devices share.
#[must_use]
pub fn find_argon_ups(devices: &[UsbDevice]) -> Option<&UsbDevice> {
    devices.iter().find(|d| {
        d.manufacturer.as_deref() == Some("Argon")
            || d.product.as_deref().is_some_and(|p| p.starts_with("Argon"))
    })
}

/// Identifies the Argon Industria Zigbee module among discovered USB devices.
///
/// The module is a CC2652P behind a CP2102N bridge. A CP2102N by itself proves nothing —
/// external USB-serial adapters are commonly the same part — so the discriminator is that
/// it sits on the case's internal USB controller.
#[must_use]
pub fn find_argon_zigbee(devices: &[UsbDevice]) -> Option<&UsbDevice> {
    devices
        .iter()
        .find(|d| d.vid == "10c4" && d.pid == "ea60" && d.on_controller(INTERNAL_USB_CONTROLLER))
}

/// The Raspberry Pi's own power button, as a kernel input device.
///
/// On a Pi 5 the dedicated power button is a `gpio-keys` device named `pwr_button`. On an
/// Argon ONE V5 the case button is simply a mechanical extension of it -- there is no Argon
/// MCU involved -- so key presses arrive as `KEY_POWER` on this device and are handled by the
/// desktop or logind, never by anything on GPIO 4.
///
/// Returns the `/dev/input/eventN` path if present.
#[must_use]
pub fn pi_power_button() -> Option<PathBuf> {
    let entries = std::fs::read_dir("/sys/class/input").ok()?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("event") {
            continue;
        }
        if read_trimmed(e.path().join("device/name")).is_ok_and(|n| n == "pwr_button") {
            return Some(PathBuf::from(format!("/dev/input/{name}")));
        }
    }
    None
}

/// The I2C bus wired to the 40-pin header, found by adapter name rather than by number.
///
/// Bus numbering depends on probe order and on which overlays are loaded. On a Pi 5 the
/// header bus is the RP1's `Synopsys DesignWare I2C adapter`; on earlier Pis it is
/// `bcm2835`. Falls back to the first bus found.
#[must_use]
pub fn header_i2c_bus() -> Option<PathBuf> {
    let buses = i2c_buses().ok()?;
    buses
        .iter()
        .find(|b| b.name.contains("DesignWare") || b.name.contains("bcm2835"))
        .or_else(|| buses.first())
        .map(|b| b.dev.clone())
}

/// The Argon UPS serial port, by a name that survives re-enumeration.
///
/// Prefers `/dev/serial/by-id/...Argon...`: a single USB re-enumeration has been observed to
/// move this device from `ttyACM0` to `ttyACM1`, which software holding the old name cannot
/// follow. Falls back to the `ttyACM` node found by USB discovery.
#[must_use]
pub fn argon_ups_serial_path() -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir("/dev/serial/by-id") {
        let mut hits: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("Argon"))
            })
            .collect();
        hits.sort();
        if let Some(p) = hits.into_iter().next() {
            return Some(p);
        }
    }
    let usb = usb_devices();
    find_argon_ups(&usb)?
        .nodes
        .iter()
        .find(|n| {
            n.file_name()
                .is_some_and(|f| f.to_string_lossy().starts_with("ttyACM"))
        })
        .cloned()
}
