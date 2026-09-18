// SPDX-License-Identifier: GPL-3.0-or-later
//! The status page argond draws on the case OLED.
//!
//! Two layers, so the part with the decisions can be tested as text: [`page`] decides what the
//! screen says, [`draw`] decides where the pixels go.
//!
//! What a status file means is not decided here. It comes from [`status::interpret`], shared
//! with the tray, so the two displays cannot disagree -- in particular about a stale file,
//! which neither may show as current.
//!
//! # Burn-in
//!
//! An OLED showing the same pixels for months wears those pixels unevenly, and a status page
//! is exactly that. Two mitigations, both cheap: the whole page moves by up to two pixels on a
//! slow cycle ([`shift_at`]), so no edge is lit in the same place for long, and the driver runs
//! the panel at a reduced contrast by default. Neither removes the wear; both spread it.

use crate::status::{self, LevelName, Reading, UpsStatus};
use argon_hal::fan_hwmon::FanReading;
use argon_proto::oled::{FrameBuffer, HEIGHT, WIDTH};
use std::time::{Duration, SystemTime};

/// Largest burn-in offset, in pixels, in each direction.
pub const MAX_SHIFT: usize = 2;

/// How long the page stays at one offset before moving to the next.
pub const SHIFT_PERIOD: Duration = Duration::from_secs(5 * 60);

/// Width available to content once the margin and the shift are allowed for.
pub const CONTENT_WIDTH: usize = WIDTH - 2 - MAX_SHIFT;

/// Characters that fit on one line of [`CONTENT_WIDTH`].
pub const LINE_CHARS: usize = CONTENT_WIDTH / 6;

/// Lines of text below the bar.
pub const MAX_LINES: usize = 4;

/// Everything the page is drawn from.
#[derive(Debug, Clone, Copy)]
pub struct PageInput<'a> {
    /// The status argond publishes, if there is one.
    pub status: Option<&'a UpsStatus>,
    /// When the page is being drawn.
    pub now: SystemTime,
    /// CPU temperature in tenths of a degree.
    pub cpu_decicelsius: Option<i32>,
    /// The kernel fan, if there is one.
    pub fan: Option<FanReading>,
}

/// What the screen says, before it becomes pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Top left, e.g. `UPS 87%`.
    pub title: String,
    /// Top right, e.g. `MAINS`.
    pub state: String,
    /// The charge bar, when there is a charge that may be shown.
    pub bar: Option<u8>,
    /// Up to [`MAX_LINES`] lines below the bar.
    pub lines: Vec<String>,
}

/// Decides what the page says. `hhmm` formats a time of day; a parameter so tests need no
/// clock or locale.
#[must_use]
pub fn page(input: &PageInput<'_>, hhmm: &dyn Fn(SystemTime) -> String) -> Page {
    let mut p = match status::interpret(input.status, input.now) {
        Reading::NoData => Page {
            title: "UPS".into(),
            state: "NO DATA".into(),
            bar: None,
            lines: vec!["No status from".into(), "argond".into()],
        },
        // No percentage and no bar: the last number argond wrote is exactly the thing that
        // must not be shown once nobody is updating it.
        Reading::Stale { age } => Page {
            title: "UPS".into(),
            state: "STALE".into(),
            bar: None,
            lines: vec![
                format!("No update for {}", short_age(age)),
                "Battery NOT watched".into(),
            ],
        },
        Reading::Failed => Page {
            title: "UPS".into(),
            state: "READ FAIL".into(),
            bar: None,
            lines: vec!["Last UPS read failed".into()],
        },
        Reading::PowerOffPending { percent, at } => Page {
            title: format!("UPS {percent}%"),
            state: "CRITICAL".into(),
            bar: Some(percent),
            lines: vec![
                format!("POWER OFF AT {}", hhmm(at)),
                "Plug in mains to".into(),
                "cancel".into(),
            ],
        },
        Reading::Current { percent, level } => Page {
            title: format!("UPS {percent}%"),
            state: match level {
                LevelName::OnMains => "MAINS",
                LevelName::OnBattery => "BATTERY",
                LevelName::Low => "LOW",
                LevelName::Critical => "CRITICAL",
                LevelName::Unrecognised(_) => "?",
            }
            .into(),
            bar: Some(percent),
            lines: Vec::new(),
        },
    };

    if let Some(line) = machine_line(input) {
        p.lines.push(line);
    }
    p.lines.truncate(MAX_LINES);
    p
}

/// An age in the largest unit that keeps it short: `45s`, `12min`, `5h`, `3d`.
///
/// A line on this panel holds 20 characters, and an outage long enough to matter overflowed
/// it in seconds.
fn short_age(age: Duration) -> String {
    let s = age.as_secs();
    match s {
        0..120 => format!("{s}s"),
        120..7_200 => format!("{}min", s / 60),
        7_200..172_800 => format!("{}h", s / 3_600),
        _ => format!("{}d", s / 86_400),
    }
}

/// `CPU 45C  FAN OFF`, or whichever half is known.
fn machine_line(input: &PageInput<'_>) -> Option<String> {
    let cpu = input.cpu_decicelsius.map(|dc| format!("CPU {}C", dc / 10));
    let fan = input.fan.map(|f| match f.rpm {
        Some(0) | None if f.pwm == 0 => "FAN OFF".to_owned(),
        Some(rpm) => format!("FAN {rpm}"),
        None => format!("FAN {}%", u32::from(f.pwm) * 100 / 255),
    });
    match (cpu, fan) {
        (Some(c), Some(f)) => Some(format!("{c}  {f}")),
        (Some(c), None) => Some(c),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    }
}

/// The burn-in offset for a moment, as `(dx, dy)`.
///
/// A slow orbit through the corners and edges of a 3x3 square, one step per
/// [`SHIFT_PERIOD`]. An orbit rather than a jump between two positions, so every offset gets
/// equal time and no single edge position dominates.
#[must_use]
pub fn shift_at(elapsed: Duration) -> (usize, usize) {
    const ORBIT: [(usize, usize); 8] = [
        (0, 0),
        (1, 0),
        (2, 0),
        (2, 1),
        (2, 2),
        (1, 2),
        (0, 2),
        (0, 1),
    ];
    let step = elapsed.as_secs() / SHIFT_PERIOD.as_secs();
    // `step % 8` is below 8, so the conversion cannot fail on any target.
    ORBIT[usize::try_from(step % 8).unwrap_or_default()]
}

/// Draws a page at a burn-in offset.
#[must_use]
pub fn draw(page: &Page, (dx, dy): (usize, usize)) -> FrameBuffer {
    let mut fb = FrameBuffer::new();
    let x = 1 + dx.min(MAX_SHIFT);
    let y = 1 + dy.min(MAX_SHIFT);

    fb.draw_text(x, y, clip(&page.title));
    let state = clip(&page.state);
    let state_x = x + CONTENT_WIDTH.saturating_sub(FrameBuffer::text_width(state));
    fb.draw_text(state_x, y, state);

    if let Some(pct) = page.bar {
        fb.bar(x, y + 10, CONTENT_WIDTH, 9, pct);
    }

    for (i, line) in page.lines.iter().take(MAX_LINES).enumerate() {
        fb.draw_text(x, y + 22 + i * 10, clip(line));
    }
    debug_assert!(
        y + 22 + (MAX_LINES - 1) * 10 + 7 <= HEIGHT,
        "layout overflows the panel"
    );
    fb
}

/// Cuts a line to what fits, so an unexpectedly long value truncates visibly at the edge
/// rather than being lost entirely by the framebuffer's own bounds check.
fn clip(s: &str) -> &str {
    match s.char_indices().nth(LINE_CHARS) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn ups(level: &str, percent: Option<u8>, shutdown_at: Option<u64>) -> UpsStatus {
        UpsStatus {
            updated: at(1_000),
            level: level.to_owned(),
            percent,
            shutdown_at: shutdown_at.map(at),
        }
    }

    fn input(status: Option<&UpsStatus>) -> PageInput<'_> {
        PageInput {
            status,
            now: at(1_005),
            cpu_decicelsius: Some(452),
            fan: Some(FanReading {
                pwm: 0,
                rpm: Some(0),
            }),
        }
    }

    fn fixed(_: SystemTime) -> String {
        "10:38".into()
    }

    #[test]
    fn a_current_reading_shows_charge_state_and_the_machine() {
        let s = ups("on-mains", Some(87), None);
        let p = page(&input(Some(&s)), &fixed);
        assert_eq!(p.title, "UPS 87%");
        assert_eq!(p.state, "MAINS");
        assert_eq!(p.bar, Some(87));
        assert_eq!(p.lines, vec!["CPU 45C  FAN OFF".to_owned()]);
    }

    #[test]
    fn a_stale_file_shows_no_percentage_and_no_bar() {
        // The case display frozen on "MAINS 95%" after argond died would be the same false
        // reassurance the tray guards against.
        let s = ups("on-mains", Some(95), None);
        let mut i = input(Some(&s));
        i.now = at(1_000) + status::STALE_AFTER + Duration::from_secs(1);
        let p = page(&i, &fixed);
        assert_eq!(p.state, "STALE");
        assert_eq!(p.bar, None);
        assert!(
            !p.title.contains("95"),
            "showed a stale percentage: {}",
            p.title
        );
        assert!(p.lines.iter().all(|l| !l.contains("95")));
        assert!(p.lines.iter().any(|l| l.contains("NOT watched")));
    }

    #[test]
    fn a_pending_poweroff_says_when_and_how_to_stop_it() {
        let s = ups("critical", Some(9), Some(2_000));
        let p = page(&input(Some(&s)), &fixed);
        assert_eq!(p.lines[0], "POWER OFF AT 10:38");
        assert!(p.lines.iter().any(|l| l.contains("mains")));
    }

    #[test]
    fn missing_and_failed_are_distinguished() {
        assert_eq!(page(&input(None), &fixed).state, "NO DATA");
        let s = ups("unknown", Some(80), None);
        let p = page(&input(Some(&s)), &fixed);
        assert_eq!(p.state, "READ FAIL");
        assert_eq!(p.bar, None, "drew a bar from a failed read");
    }

    #[test]
    fn every_line_the_page_can_produce_fits_the_panel() {
        // Enumerated from the page's own outputs, not a hand-written list of strings, so a
        // new message that is too long fails here rather than being cut off on the case.
        let long_ago = at(1_000) + Duration::from_secs(9_999_999);
        let cases = [
            (None, at(1_005)),
            (Some(ups("on-mains", Some(100), None)), at(1_005)),
            (Some(ups("on-battery", Some(55), None)), at(1_005)),
            (Some(ups("low", Some(18), None)), at(1_005)),
            (Some(ups("critical", Some(9), None)), at(1_005)),
            (Some(ups("critical", Some(9), Some(2_000))), at(1_005)),
            (Some(ups("unknown", None, None)), at(1_005)),
            (Some(ups("on-mains", Some(100), None)), long_ago),
        ];
        for (status, now) in &cases {
            let mut i = input(status.as_ref());
            i.now = *now;
            i.fan = Some(FanReading {
                pwm: 255,
                rpm: Some(12_345),
            });
            i.cpu_decicelsius = Some(1_050);
            let p = page(&i, &|_| "23:59".to_owned());
            let title_and_state =
                FrameBuffer::text_width(&p.title) + 6 + FrameBuffer::text_width(&p.state);
            assert!(title_and_state <= CONTENT_WIDTH, "top line too long: {p:?}");
            for line in &p.lines {
                assert!(
                    line.chars().count() <= LINE_CHARS,
                    "line too long: {line:?}"
                );
            }
            assert!(p.lines.len() <= MAX_LINES);
        }
    }

    #[test]
    fn ages_use_the_largest_unit_that_stays_short() {
        assert_eq!(short_age(Duration::from_secs(61)), "61s");
        assert_eq!(short_age(Duration::from_secs(600)), "10min");
        assert_eq!(short_age(Duration::from_secs(5 * 3_600)), "5h");
        assert_eq!(short_age(Duration::from_secs(400 * 86_400)), "400d");
    }

    #[test]
    fn the_shift_orbits_within_bounds_and_changes_the_pixels() {
        let mut seen = std::collections::HashSet::new();
        for step in 0..16 {
            let (dx, dy) = shift_at(SHIFT_PERIOD * step);
            assert!(dx <= MAX_SHIFT && dy <= MAX_SHIFT);
            seen.insert((dx, dy));
        }
        assert_eq!(seen.len(), 8, "the orbit does not visit every position");

        let s = ups("on-mains", Some(87), None);
        let p = page(&input(Some(&s)), &fixed);
        let a = draw(&p, (0, 0));
        let b = draw(&p, (2, 2));
        assert_ne!(a, b, "shifting did not move anything");
        assert_eq!(
            a.lit_pixels(),
            b.lit_pixels(),
            "shifting clipped part of the page"
        );
    }

    #[test]
    fn nothing_is_drawn_outside_the_margin() {
        // A one-pixel frame is kept clear at every offset; lit pixels there would mean the
        // layout arithmetic has drifted and content is being clipped by the panel edge.
        let s = ups("critical", Some(100), Some(2_000));
        let mut i = input(Some(&s));
        i.fan = Some(FanReading {
            pwm: 255,
            rpm: Some(12_345),
        });
        let p = page(&i, &|_| "23:59".to_owned());
        for shift in [(0, 0), (MAX_SHIFT, MAX_SHIFT)] {
            let fb = draw(&p, shift);
            for x in 0..WIDTH {
                assert!(!fb.pixel(x, 0), "lit pixel on the top edge at {x}");
                assert!(
                    !fb.pixel(x, HEIGHT - 1),
                    "lit pixel on the bottom edge at {x}"
                );
            }
            for y in 0..HEIGHT {
                assert!(!fb.pixel(0, y), "lit pixel on the left edge at {y}");
                assert!(
                    !fb.pixel(WIDTH - 1, y),
                    "lit pixel on the right edge at {y}"
                );
            }
        }
    }
}
