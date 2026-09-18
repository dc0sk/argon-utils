// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl zigbee` — the Industria Zigbee module: which device it is, and (task T16) whether
//! its firmware answers.
//!
//! Without `--probe` nothing is opened at all. With it, the probe sends exactly two
//! side-effect-free requests, `SYS_PING` and `SYS_VERSION`, after first listening in silence.
//!
//! # Why listen first
//!
//! On many CC2652 boards the USB-serial bridge's DTR and RTS lines drive the radio's reset and
//! bootloader pins, and Linux raises both whenever a serial port is opened. Nobody has
//! documented this module's wiring. So the probe lowers DTR, then RTS, straight after opening
//! -- releasing a bootloader line before a reset line makes the radio restart into its normal
//! firmware, not the bootloader -- and then listens for three seconds. Z-Stack announces every
//! restart with `SYS_RESET_IND`, so an announcement in that window is an observation of the
//! wiring: opening the port restarted the radio.

use argon_hal::discovery;
use argon_hal::foreign;
use argon_hal::serial::RawPort;
use argon_proto::zigbee::{
    MtFrame, MtReader, SYS_PING, SYS_PING_RSP, SYS_RESET_IND, SYS_VERSION, SYS_VERSION_RSP, Version,
};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// Z-Stack's line rate (`ZIGBEE-BAUD`, documented in TI's MT API; not yet observed here).
const BAUD: u32 = 115_200;
/// How long to listen, sending nothing, after opening.
const LISTEN: Duration = Duration::from_secs(3);
/// How long to wait for each reply.
const REPLY: Duration = Duration::from_secs(1);

#[derive(clap::Args)]
pub struct Args {
    /// Run the task T16 health probe: listen, then ping and ask for the firmware version.
    #[arg(long)]
    pub probe: bool,

    /// Serial port. Found by the module's position on the case's internal USB hub if not given.
    #[arg(long, value_name = "PATH")]
    pub port: Option<PathBuf>,
}

pub fn run(args: &Args) -> ExitCode {
    let path = if let Some(p) = &args.port {
        p.clone()
    } else {
        let devices = discovery::usb_devices();
        let Some(z) = discovery::find_argon_zigbee(&devices) else {
            eprintln!(
                "argonctl: no Zigbee module found on the case's internal USB hub. Is it \
                 fitted, and is dtoverlay=dwc2,dr_mode=host set in config.txt?"
            );
            return ExitCode::FAILURE;
        };
        println!("Zigbee module on the internal hub, USB {}", z.kernel);
        println!(
            "  bridge           {} {} (serial {})",
            z.manufacturer.as_deref().unwrap_or("?"),
            z.product.as_deref().unwrap_or("?"),
            z.serial.as_deref().unwrap_or("none")
        );
        let Some(node) = z
            .nodes
            .iter()
            .find(|n| n.to_string_lossy().contains("ttyUSB"))
        else {
            eprintln!("argonctl: the module has no serial node");
            return ExitCode::FAILURE;
        };
        node.clone()
    };
    println!("  port             {}", path.display());

    let owners = foreign::port_owners(&path);
    match owners.first() {
        Some(o) => println!("  in use by        pid {} ({})", o.pid, o.comm),
        None if foreign::can_see_all_processes() => println!("  in use by        nothing"),
        None => {
            println!("  in use by        nothing visible (run under sudo to see every process)");
        }
    }

    if !args.probe {
        println!();
        println!("Nothing was opened. The health probe is task T16:  argonctl zigbee --probe");
        return ExitCode::SUCCESS;
    }
    if let Some(o) = owners.first() {
        // A coordinator belongs to whatever runs the network. Probing it underneath zigbee2mqtt
        // or ZHA would inject frames into a live session.
        eprintln!(
            "\nargonctl: {} is held by pid {} ({}); not probing a coordinator that something \
             else is running.",
            path.display(),
            o.pid,
            o.comm
        );
        return ExitCode::FAILURE;
    }

    match probe(&path) {
        Ok(findings) => {
            report(&findings);
            if findings.ping.is_some() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("argonctl: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The raw bytes of one probe.
struct Capture {
    listened: Vec<u8>,
    ping: Vec<u8>,
    version: Vec<u8>,
}

fn probe(path: &std::path::Path) -> argon_hal::Result<Findings> {
    println!();
    println!("Step 1: open at {BAUD} baud, then lower DTR and RTS, in that order");
    let mut port = RawPort::open(path, BAUD)?;
    port.set_lines(false, false)?;

    println!("Step 2: listen for {} s, sending nothing", LISTEN.as_secs());
    let listened = port.collect(LISTEN)?;
    print_bytes("heard", &listened);

    println!("Step 3: SYS_PING");
    let ping = exchange(&mut port, SYS_PING)?;

    println!("Step 4: SYS_VERSION");
    let version = exchange(&mut port, SYS_VERSION)?;

    // Closing drops DTR and RTS, which are already low: no transition, so no second restart.
    Ok(interpret(&Capture {
        listened,
        ping,
        version,
    }))
}

fn exchange(port: &mut RawPort, cmd: (u8, u8)) -> argon_hal::Result<Vec<u8>> {
    let mut out = [0u8; 16];
    let n = MtFrame::new(cmd, &[])
        .and_then(|f| f.encode_into(&mut out))
        .unwrap_or(0);
    print_bytes("sent", &out[..n]);
    port.write_all(&out[..n])?;
    let got = port.collect(REPLY)?;
    print_bytes("received", &got);
    Ok(got)
}

fn print_bytes(label: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        println!("  {label:<16} nothing");
    } else {
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
        println!("  {label:<16} {} ({} bytes)", hex.join(" "), bytes.len());
    }
}

/// What the probe established.
#[derive(Debug, Default, PartialEq, Eq)]
struct Findings {
    /// A `SYS_RESET_IND` arrived while listening: opening the port restarted the radio. Its
    /// reason byte, if one was sent.
    reset_on_open: Option<u8>,
    /// Any bytes at all arrived while listening.
    heard_anything: bool,
    /// The `SYS_PING` reply's capability bits, if it answered.
    ping: Option<u16>,
    /// The firmware version, if it answered.
    version: Option<Version>,
    /// Frames that failed their check byte, across the whole probe.
    rejected: usize,
}

fn frames(bytes: &[u8], rejected: &mut usize) -> Vec<MtFrame> {
    let mut r = MtReader::new();
    let mut out = Vec::new();
    for &b in bytes {
        match r.push(b) {
            Some(Ok(f)) => out.push(f),
            Some(Err(_)) => *rejected += 1,
            None => {}
        }
    }
    out
}

fn interpret(c: &Capture) -> Findings {
    let mut rejected = 0;
    let heard = frames(&c.listened, &mut rejected);
    let reset_on_open = heard
        .iter()
        .find(|f| f.is(SYS_RESET_IND))
        .map(|f| f.data().first().copied().unwrap_or(0xFF));
    let ping = frames(&c.ping, &mut rejected)
        .into_iter()
        .find(|f| f.is(SYS_PING_RSP))
        .and_then(|f| match *f.data() {
            [lo, hi] => Some(u16::from_le_bytes([lo, hi])),
            _ => None,
        });
    let version = frames(&c.version, &mut rejected)
        .into_iter()
        .find(|f| f.is(SYS_VERSION_RSP))
        .and_then(|f| Version::decode(f.data()));
    Findings {
        reset_on_open,
        heard_anything: !c.listened.is_empty(),
        ping,
        version,
        rejected,
    }
}

fn report(f: &Findings) {
    println!();
    println!("T16 RESULT");
    match f.reset_on_open {
        Some(reason) => println!(
            "  Opening the port RESTARTED the radio (SYS_RESET_IND, reason 0x{reason:02x}): DTR/RTS \
             reach its reset line."
        ),
        None if f.heard_anything => {
            println!("  Bytes arrived while listening, but no restart announcement among them.");
        }
        None => println!("  No restart announcement: opening the port did not visibly restart it."),
    }
    if let Some(caps) = f.ping {
        println!("  Firmware ANSWERS: SYS_PING replied, capabilities 0x{caps:04x}.");
    } else {
        println!("  NO ANSWER to SYS_PING.");
        println!("  Possible causes, none tested: a line rate other than {BAUD}, firmware that");
        println!("  does not speak Z-Stack MT, or a radio held in reset or in its bootloader.");
        println!("  Nothing else was sent. DTR and RTS were left low.");
    }
    if let Some(v) = f.version {
        print!(
            "  Firmware version: product {}, release {}.{}.{}, transport rev {}",
            v.product, v.major, v.minor, v.maint, v.transport_rev
        );
        match v.revision {
            Some(r) => println!(", revision {r}"),
            None => println!(),
        }
    } else if f.ping.is_some() {
        println!("  SYS_VERSION did not return a decodable reply.");
    }
    if f.rejected > 0 {
        println!("  {} frame(s) failed their check byte.", f.rejected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cmd: (u8, u8), data: &[u8]) -> Vec<u8> {
        let mut out = [0u8; 32];
        let n = MtFrame::new(cmd, data)
            .unwrap()
            .encode_into(&mut out)
            .unwrap();
        out[..n].to_vec()
    }

    #[test]
    fn a_healthy_coordinator_that_restarted_on_open() {
        let rev = 20_230_507u32.to_le_bytes();
        let f = interpret(&Capture {
            listened: frame(SYS_RESET_IND, &[0x00, 2, 1, 2, 7, 1]),
            ping: frame(SYS_PING_RSP, &[0x59, 0x06]),
            version: frame(
                SYS_VERSION_RSP,
                &[2, 1, 2, 7, 1, rev[0], rev[1], rev[2], rev[3]],
            ),
        });
        assert_eq!(f.reset_on_open, Some(0x00));
        assert_eq!(f.ping, Some(0x0659));
        assert_eq!(f.version.unwrap().revision, Some(20_230_507));
        assert_eq!(f.rejected, 0);
    }

    #[test]
    fn a_healthy_coordinator_that_did_not_restart() {
        let f = interpret(&Capture {
            listened: Vec::new(),
            ping: frame(SYS_PING_RSP, &[0x59, 0x06]),
            version: frame(SYS_VERSION_RSP, &[2, 1, 2, 7, 1]),
        });
        assert_eq!(f.reset_on_open, None);
        assert!(!f.heard_anything);
        assert!(f.ping.is_some());
    }

    #[test]
    fn silence_is_no_answer_not_an_error() {
        let f = interpret(&Capture {
            listened: Vec::new(),
            ping: Vec::new(),
            version: Vec::new(),
        });
        assert_eq!(f, Findings::default());
    }

    #[test]
    fn a_reply_to_the_wrong_request_is_not_taken_as_an_answer() {
        // A version response arriving where the ping's should be proves the firmware talks,
        // but it is not a ping reply, and must not be reported as one.
        let f = interpret(&Capture {
            listened: Vec::new(),
            ping: frame(SYS_VERSION_RSP, &[2, 1, 2, 7, 1]),
            version: Vec::new(),
        });
        assert_eq!(f.ping, None);
    }

    #[test]
    fn corrupted_frames_are_counted_not_believed() {
        let mut bad = frame(SYS_PING_RSP, &[0x59, 0x06]);
        *bad.last_mut().unwrap() ^= 0xFF;
        let f = interpret(&Capture {
            listened: Vec::new(),
            ping: bad,
            version: Vec::new(),
        });
        assert_eq!(f.ping, None);
        assert_eq!(f.rejected, 1);
    }

    #[test]
    fn noise_while_listening_is_not_a_restart() {
        let f = interpret(&Capture {
            listened: vec![0x00, 0x55, 0x13],
            ping: Vec::new(),
            version: Vec::new(),
        });
        assert!(f.heard_anything);
        assert_eq!(f.reset_on_open, None);
    }
}
