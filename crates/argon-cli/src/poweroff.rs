// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl poweroff --wake-at TIME` — power off now, and have the UPS power the machine on
//! again at TIME.
//!
//! argond does the work, because it owns the UPS port: it sets the wake, reads it back, and
//! only then schedules the poweroff, a minute out. This command asks it to, over the control
//! socket, and reports what happened.

use argon_device::control::{self, Request, Response};
use argon_device::wake;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(clap::Args)]
pub struct Args {
    /// When the UPS should power the machine on again, in local time: anything GNU `date -d`
    /// understands, such as "07:00", "tomorrow 07:00" or "2026-09-20 06:30". A bare time of day
    /// means the next one.
    #[arg(long, value_name = "TIME")]
    pub wake_at: String,

    /// Show what would happen, and do nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: &Args) -> ExitCode {
    let now = now_unix();
    let at = match resolve(&args.wake_at, now) {
        Ok(at) => at,
        Err(e) => {
            eprintln!("argonctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    let schedule = match wake::schedule_for(at, now) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("argonctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    let wake_unix = schedule.to_unix_seconds().unwrap_or(at);

    println!("Plan:");
    println!(
        "  power off        in {} s, announced to logged-in users",
        wake::POWEROFF_DELAY.as_secs()
    );
    println!(
        "  wake             {}   ({:04}-{:02}-{:02} {:02}:{:02} UTC, as the UPS holds it)",
        local(wake_unix),
        schedule.year,
        schedule.month,
        schedule.day,
        schedule.hour,
        schedule.minute
    );
    if args.dry_run {
        println!("\nDry run: nothing was sent.");
        return ExitCode::SUCCESS;
    }

    match ask(&Request::PoweroffWithWake { at_unix: at }) {
        Ok(Response::PoweroffScheduled {
            wake_unix,
            poweroff_unix,
        }) => {
            println!();
            println!("Done. The wake is set and was read back from the UPS.");
            println!("  powering off at  {}", local(poweroff_unix));
            println!("  waking at        {}", local(wake_unix));
            println!();
            println!("To stay up after all:  sudo shutdown -c");
            println!("argond then moves the wake out of the way before it can come due.");
            ExitCode::SUCCESS
        }
        Ok(Response::Error { message }) => {
            eprintln!("argonctl: argond refused: {message}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("argonctl: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolves a local time with GNU `date`. A bare time of day that has already passed today --
/// or is too close to be accepted -- means the same time tomorrow.
fn resolve(input: &str, now: u64) -> Result<u64, String> {
    let at = date_to_unix(input)?;
    let bare_time_of_day = input.trim().chars().all(|c| c.is_ascii_digit() || c == ':');
    if bare_time_of_day && at < now + wake::MIN_LEAD.as_secs() {
        return date_to_unix(&format!("tomorrow {}", input.trim()));
    }
    Ok(at)
}

fn date_to_unix(input: &str) -> Result<u64, String> {
    let out = Command::new("date")
        .args(["-d", input, "+%s"])
        .output()
        .map_err(|e| format!("cannot run date: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cannot understand {input:?} as a time: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| format!("date gave no timestamp for {input:?}"))
}

/// A unix time as local wall-clock time, via `date`.
fn local(unix: u64) -> String {
    Command::new("date")
        .args(["-d", &format!("@{unix}"), "+%a %Y-%m-%d %H:%M %Z"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || format!("unix {unix}"),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        )
}

fn ask(req: &Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(control::SOCKET_PATH).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => format!(
            "{} is argond's, and only root may use it: run this under sudo",
            control::SOCKET_PATH
        ),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => format!(
            "argond is not listening on {} -- is the argond service running?",
            control::SOCKET_PATH
        ),
        _ => format!("cannot reach argond: {e}"),
    })?;
    let line = control::to_line(req).map_err(|e| e.to_string())?;
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("sending the request: {e}"))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(40)));
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .map_err(|e| format!("waiting for argond's answer: {e}"))?;
    control::from_line(&answer).map_err(|e| format!("argond's answer made no sense: {e}"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_time_already_past_means_tomorrow() {
        // One minute ago, as HH:MM, must resolve to roughly a day from now, not the past.
        let now = now_unix();
        let hhmm = Command::new("date")
            .args(["-d", &format!("@{}", now - 60), "+%H:%M"])
            .output()
            .unwrap();
        let hhmm = String::from_utf8_lossy(&hhmm.stdout).trim().to_owned();
        let at = resolve(&hhmm, now).unwrap();
        assert!(
            at > now + 23 * 3_600 && at < now + 25 * 3_600,
            "{hhmm} resolved to {} s from now",
            at.saturating_sub(now)
        );
    }

    #[test]
    fn an_explicit_date_is_not_moved() {
        let now = now_unix();
        let at = resolve("2099-01-02 03:04", now).unwrap();
        assert!(at > now + 365 * 86_400);
    }

    #[test]
    fn nonsense_is_an_error() {
        assert!(resolve("half past nowhere", now_unix()).is_err());
    }
}
