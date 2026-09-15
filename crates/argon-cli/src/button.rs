// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl button watch` — measure the case button's pulse widths.
//!
//! The case button is wired to the MCU, not to the Pi. The MCU decodes the press and emits a
//! calibrated pulse on BCM 4, whose *width* encodes which action was requested. So the host
//! is not timing a human finger; it is measuring a signal the MCU generates.
//!
//! This exists because the vendor's thresholds are held as `inferred` in
//! `docs/protocol/FACTS.md` and are not trusted: they were derived from a `sleep(0.01)`
//! accumulation loop, which drifts under load and cannot resolve the windows it claims to.
//! Here the kernel timestamps each edge in nanoseconds, so the measurement is bounded by the
//! hardware rather than by scheduler latency.

use argon_hal::{discovery, gpio};
use std::process::ExitCode;
use std::time::Duration;

/// The line the MCU pulses to signal the host.
const BUTTON_LINE: u32 = 4;

#[derive(clap::Args)]
pub struct Args {
    /// Stop after this many pulses.
    #[arg(long, default_value = "30")]
    pub count: usize,

    /// Give up after this long with no further pulses.
    #[arg(long, default_value = "300", value_name = "SECONDS")]
    pub timeout: u64,

    /// Line offset to watch, if not the default.
    #[arg(long, default_value_t = BUTTON_LINE)]
    pub line: u32,
}

pub fn run(args: &Args) -> ExitCode {
    let chips = discovery::gpio_chips();
    let Some(chip) = chips
        .iter()
        .find(|c| c.label.contains("rp1") || c.label.contains("bcm2835"))
    else {
        eprintln!("argonctl: could not identify the header GPIO chip by label");
        return ExitCode::FAILURE;
    };

    // Refuse to fight another process for the line. Two consumers on an edge-event line do
    // not share it; one of them simply loses.
    let lines = discovery::gpio_lines(&chip.path, &[args.line]);
    if let Some(l) = lines.first() {
        if let Some(consumer) = &l.consumer {
            eprintln!(
                "argonctl: line {} is held by `{consumer}`.\n\
                 Stop that process first, or pass --line to watch a different one.",
                args.line
            );
            return ExitCode::FAILURE;
        }
    }

    let watcher = match gpio::EdgeWatcher::open(&chip.path, args.line, "argonctl-button-watch") {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "argonctl: cannot watch line {} on {}: {e}",
                args.line,
                chip.path.display()
            );
            eprintln!(
                "\nGPIO character devices are root:gpio. Run with sudo, or join the gpio group."
            );
            return ExitCode::FAILURE;
        }
    };

    println!(
        "Watching {} line {} for button pulses.",
        chip.label, args.line
    );
    println!("Press the case power button. Mix short and longer presses.");
    println!(
        "Stopping after {} pulses or {}s of silence. Ctrl-C to stop early.\n",
        args.count, args.timeout
    );

    let mut widths_us: Vec<u64> = Vec::new();
    let mut rising_at: Option<u64> = None;

    while widths_us.len() < args.count {
        let event = match watcher.next_edge(Duration::from_secs(args.timeout)) {
            Ok(Some(e)) => e,
            Ok(None) => {
                println!("\n(no pulse for {}s — stopping)", args.timeout);
                break;
            }
            Err(e) => {
                eprintln!("argonctl: edge wait failed: {e}");
                break;
            }
        };

        match event.edge {
            gpio::Edge::Rising => rising_at = Some(event.timestamp_ns),
            gpio::Edge::Falling => {
                if let Some(start) = rising_at.take() {
                    let us = event.timestamp_ns.saturating_sub(start) / 1_000;
                    widths_us.push(us);
                    println!(
                        "  pulse {:>3}: {:>7} us  ({} ms)",
                        widths_us.len(),
                        us,
                        fmt_ms(us)
                    );
                }
            }
        }
    }

    if widths_us.is_empty() {
        println!("\nNo pulses captured.");
        println!(
            "If the button did nothing at all, that is itself the finding -- see task T2 in\n\
             docs/testing/HUMAN-TASKS.md."
        );
        return ExitCode::SUCCESS;
    }

    report(&widths_us);
    ExitCode::SUCCESS
}

/// Prints a histogram and proposes thresholds with margins.
fn report(widths_us: &[u64]) {
    let mut sorted = widths_us.to_vec();
    sorted.sort_unstable();

    println!("\nDistribution ({} pulses)", sorted.len());
    println!("-------------------------");

    // Cluster by gaps: a gap much larger than the local spread separates two intended
    // widths. Deliberately simple -- the point is to show the operator the structure, not to
    // decide thresholds automatically from 30 samples.
    let mut clusters: Vec<Vec<u64>> = Vec::new();
    for &w in &sorted {
        match clusters.last_mut() {
            Some(c) if w.saturating_sub(*c.last().unwrap_or(&0)) <= 5_000 => c.push(w),
            _ => clusters.push(vec![w]),
        }
    }

    for c in &clusters {
        let lo = *c.first().unwrap_or(&0);
        let hi = *c.last().unwrap_or(&0);
        let mid = c.iter().sum::<u64>() / c.len().max(1) as u64;
        println!(
            "  {:>3} pulse(s)  {:>7}..{:<7} us   mean {:>7} us  ({}..{} ms)",
            c.len(),
            lo,
            hi,
            mid,
            fmt_ms(lo),
            fmt_ms(hi)
        );
    }

    println!("\nProposed thresholds");
    println!("-------------------");
    if clusters.len() < 2 {
        println!("  Only one cluster. Press a wider variety of durations, or the MCU emits");
        println!("  a single pulse width and the action is encoded some other way.");
        return;
    }
    for pair in clusters.windows(2) {
        let hi = *pair[0].last().unwrap_or(&0);
        let lo = *pair[1].first().unwrap_or(&0);
        let gap = lo.saturating_sub(hi);
        let boundary = hi + gap / 2;
        println!(
            "  boundary at {:>7} us ({} ms), margin +/-{} ms",
            boundary,
            fmt_ms(boundary),
            fmt_ms(gap / 2)
        );
    }
    println!(
        "\nRecord these in docs/protocol/one-button.md and promote ARGON-GPIO-BTN-WINDOWS\n\
         from `inferred` to `observed`."
    );
}

/// Formats microseconds as milliseconds with one decimal, using integer arithmetic.
///
/// Avoids a `u64 as f64` cast. Pulse widths are far below the point where that would
/// actually lose precision, but the cast would still need silencing, and silencing a
/// precision lint by habit is how a real one gets missed later.
fn fmt_ms(us: u64) -> String {
    format!("{}.{}", us / 1000, (us % 1000) / 100)
}
