---
project: argon-utils
doc: protocol/captures/OBS-2026-09-17-ups-serial-capture
status: evidence
last_updated: 2026-09-17
---

# OBS-2026-09-17-ups-serial-capture — the UPS serial protocol, confirmed on hardware

Task T4. **The Argon PWR UPS serial protocol works as specified**, and the framing, checksum
and commands 0, 2, 4, 5 and 7 are promoted from `inferred` to `observed`.

## Method

The vendor's `argonupsrtcd` was stopped for the duration and restarted afterwards; CDC-ACM has
no arbitration, so two readers would have corrupted each other's frames. **Read-only commands
only** — battery status, charge current, firmware version, RTC and wake schedule. No writes,
and specifically no meter reset, which would have discarded the battery meter's baseline.

Every response's trailing checksum was verified against an independent hand computation
before anything was committed.

| | |
|---|---|
| Device | Argon PWR UPS, firmware 113, serial `XXXXXXXXXXXXXXXX` |
| Host | Raspberry Pi 5 Model B Rev 1.1, Debian 13 trixie, kernel 6.18.39+rpt-rpi-2712 |
| Link | `/dev/ttyACM0`, 115200 8N1 |
| State | on mains, charging, 91% |

## Captured exchanges

Committed as [`ups-reads.json`](../../../crates/argon-proto/tests/tapes/ups-reads.json) and
asserted against in `argon-proto`'s frame tests.

| Cmd | Request | Response | Decoded |
|---|---|---|---|
| 0 battery | `fe 00 00 fe` | `fe 02 00 5b 00 5b` | 91%, charging byte 0 → on mains |
| 4 firmware | `fe 00 04 02` | `fe 01 04 71 74` | 113 |
| 5 RTC | `fe 00 05 03` | `fe 06 05 26 09 17 13 29 15 a0` | 2026-09-17 13:29:15 UTC |
| 7 wake | `fe 00 07 05` | `fe 00 07 05` | **empty payload — no schedule set** |
| 2 current | `fe 00 02 00` | `fe 02 02 03 52 57` | raw 16-bit BE = 850, units unknown |

Battery percentage and firmware version both match what the vendor's daemon independently
reports, which is a cross-check rather than a self-consistent one.

## Two things worth recording

### The synthetic test frame was exactly right

`argon-proto` carried a test written before any capture existed, predicting the
firmware-version exchange purely from the framing rules, and explicitly labelled as *not* a
capture. It predicted `FE 01 04 71 74`. The device sent `fe 01 04 71 74`.

That is a pleasing result, but the reason it is recorded here is the discipline rather than
the luck: the test said plainly that it was synthetic, so when the real bytes arrived there
was no ambiguity about what had been verified and what had merely been assumed. A test that
had quietly claimed to be a capture would have proved nothing either way.

### An empty payload is a valid answer

With no wake schedule set, command 7 replies `FE 00 07 05` — a well-formed frame carrying no
payload at all. The implementation expected five BCD bytes and reported
`undecodable (expected a 5-byte payload, got 0)`, i.e. a decode failure on a perfectly healthy
device.

Fixed by `UpsTime::decode_optional_schedule`, which treats an empty payload as "none set".
Recorded as `ARGON-UPS-CMD7-EMPTY`. This is exactly the class of detail that only hardware
tells you, and the reason `inferred` facts are not permitted to back a write path.

## Still unknown

The **units of command 2**. It read 850 while charging at 91%. That is consistent with
milliamps, and also with several other things. Resolving it needs a controlled load
comparison against an inline power meter, with a negative control — not a guess. Until then
it is exposed as a raw value with no unit and no Home Assistant device class.
