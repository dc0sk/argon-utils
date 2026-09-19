// SPDX-License-Identifier: GPL-3.0-or-later
//! Renders the OLED status page for a few example states, as PBM images, for the documentation.
//!
//! The pixels come from the same `page()` and `draw()` argond uses, so the pictures are the real
//! page, fed with made-up readings -- nothing from any real machine.
//!
//!     cargo run -p argon-device --example oled_pages -- OUT_DIR
//!
//! `scripts/screenshots.sh` turns them into the PNGs under `site/img/`.

use argon_device::oled_page::{PageInput, draw, page};
use argon_device::status::UpsStatus;
use argon_hal::fan_hwmon::FanReading;
use argon_proto::oled::{HEIGHT, WIDTH};
use std::io::Write;
use std::time::{Duration, SystemTime};

fn main() -> std::io::Result<()> {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out)?;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let reading = |level: &str, percent: u8, shutdown: Option<u64>| UpsStatus {
        updated: now,
        level: level.to_owned(),
        percent: Some(percent),
        shutdown_at: shutdown.map(|s| now + Duration::from_secs(s)),
    };
    let states = [
        ("oled-mains", reading("on-mains", 92, None)),
        ("oled-battery", reading("on-battery", 64, None)),
        ("oled-critical", reading("critical", 9, Some(120))),
    ];
    for (name, s) in &states {
        let input = PageInput {
            status: Some(s),
            now,
            cpu_decicelsius: Some(512),
            fan: Some(FanReading {
                pwm: 75,
                rpm: Some(2_914),
            }),
        };
        let fb = draw(&page(&input, &|_| "21:42".to_owned()), (0, 0));
        let mut f = std::fs::File::create(out.join(format!("{name}.pbm")))?;
        writeln!(f, "P1\n{WIDTH} {HEIGHT}")?;
        for y in 0..HEIGHT {
            let row: Vec<&str> = (0..WIDTH)
                .map(|x| if fb.pixel(x, y) { "1" } else { "0" })
                .collect();
            writeln!(f, "{}", row.join(" "))?;
        }
    }
    Ok(())
}
