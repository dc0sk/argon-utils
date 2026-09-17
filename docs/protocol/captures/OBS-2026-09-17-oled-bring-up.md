---
project: argon-utils
doc: protocol/captures/OBS-2026-09-17-oled-bring-up
status: evidence
last_updated: 2026-09-17
---

# OBS-2026-09-17-oled-bring-up

Task T7. **The OLED module on the Argon ONE V5 is a 128x64 SSD1306 at I2C `0x3c`, mounted in
the normal orientation. It works with the initialisation sequence written from the datasheet.**

## Method

`argonctl oled --write` with `argononed` stopped: the full datasheet init sequence, the
normal orientation remap (`0xA1`, `0xC8`), then the T7 test pattern flushed in page-sized
chunks. The panel was blanked with `argonctl oled --off` afterwards, and `argononed`
was restarted.

## Result

The user reported that the panel showed the pattern correctly. No photograph was committed.
The evidence is that visual confirmation.

Because the pattern was designed to make each property visible, "shows the pattern
correctly" establishes:

| Property | How the pattern shows it | Result |
|---|---|---|
| Panel initialises | Anything visible at all; the charge pump starts off at power-up | ✔ |
| Orientation | Readable text, solid box top-left | ✔ normal, no `--flip` |
| Full 128x64 addressable | Unbroken one-pixel border | ✔ |
| SSD1306, not SH1106 | No two-pixel shift or edge garbage | ✔ |
| Partial fills | 60% bar | ✔ |

| | |
|---|---|
| Host | Raspberry Pi 5 Model B Rev 1.1, Debian 13 trixie, in an Argon ONE V5 |
| Bus | `i2c-1`, Synopsys DesignWare adapter |
| Date | 2026-09-17 |

## What it does not establish

It shows one static frame. Repeated flushes, page cycling, the screensaver and long-run
behaviour are untested on hardware, as is whether the panel keeps its settings across a
power cycle.
