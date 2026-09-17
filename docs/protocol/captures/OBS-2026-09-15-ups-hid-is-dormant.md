---
project: argon-utils
doc: protocol/captures/OBS-2026-09-15-ups-hid-is-dormant
status: evidence
last_updated: 2026-09-15
---

# OBS-2026-09-15-ups-hid-is-dormant — the UPS HID interface serves no data

Resolves `ARGON-UPS-HID-LIVE`. **The Argon PWR UPS publishes a complete and valid HID Power
Device descriptor, but firmware 113 appears not to serve any data through it.**

This corrects an earlier conclusion in this project. The descriptor's contents
(`OBS-2026-09-15-ups-hid-descriptor`) are real and correctly parsed — but a descriptor
describes what a device *claims*, not what it *does*, and the two differ here.

## What was tested

All read-only, all via `hidraw` or `hiddev`. Neither claims a USB interface, so unlike the
NUT experiment nothing was disturbed.

| Probe | Result |
|---|---|
| `HIDIOCGFEATURE` on reports `0x0c`, `0x07`, `0x11`, `0x17`, `0x0d`, `0x0e`, `0x1c`, `0x0b` | returns **1 byte** — the echoed report ID, no payload — for every buffer size from 2 to 64 |
| `HIDIOCGFEATURE` with report ID 0 | `EPIPE`, correctly: this is a numbered-report device |
| Raw `read()` on `/dev/hidraw0`, 45 s | **no Input report at all** |
| `hiddev` `HIDIOCINITREPORT` + per-report `HIDIOCGREPORT`/`HIDIOCGUSAGE` | every value **0** |
| `/sys/class/power_supply` | empty — the kernel bound `hid-generic`, created no battery |

## Why the probe itself is trustworthy

An empty result is only meaningful if the instrument works, so:

- The same `HIDIOCGFEATURE` code returns **different** errors on the machine's other HID
  devices — `EPIPE` on one, `EOVERFLOW` on another — and success-with-ID on the UPS. A
  malformed ioctl would fail uniformly.
- `hiddev` enumeration returned the **correct structure**: report `0x0c` with one field of
  range 0..100, `0x1c` and `0x0d` of 0..65535, `0x08` of 120..1380, `0x14` of 1..3, and
  `0x07` with fourteen single-bit fields. That is exactly what our own parser produces from
  the same bytes, so the kernel and this project independently agree on the descriptor. The
  transport works; the data is absent.

## Independent validation of our descriptor parser

Worth recording separately, because it is a genuine cross-check rather than a
self-consistent one: the **Linux kernel's** HID parser and `argon-proto`'s parser, given the
same 416 bytes, derive the same report IDs, field counts, and logical minima and maxima.
Two implementations written by different people from the same public specification agree.
That is the strongest evidence available that the parser is correct, and it arrived by
accident while testing something else.

## What this changes

The project previously recorded that the HID channel supplies runtime-to-empty, capacities
and voltage that the serial protocol cannot, and that HID should therefore be *preferred*
for telemetry. **That is withdrawn.** The correct statement is:

> The HID interface advertises a full Power Device and is dormant on firmware 113. The
> Argon-proprietary serial protocol is the only channel observed to return live data, which
> is consistent with the vendor's own software using it exclusively.

The descriptor parser is kept: it is correct, validated, and costs nothing to retain. Its
use is gated behind actually receiving data, so if a later firmware activates the interface
the capability appears without further work.

## The state-change test — done 2026-09-17, and it confirms the finding

HID Power Devices commonly send Input reports **on state change** rather than on a timer, so
every probe above being taken with the UPS stable on mains left one way out: that the
interface works and simply had nothing to say.

Mains was physically disconnected while `argonctl ups --wait 60` listened, with the vendor's
serial daemon watched in parallel as an independent check that the transition really happened.

| Channel | Observation |
|---|---|
| Serial (vendor daemon) | `Power:Battery 93%` — the transition was seen immediately and held for the full window |
| HID Input reports | **none, across 60 s spanning the transition** |
| HID Feature reads, while on battery | still 1 byte, the echoed report ID, no data |

The transition demonstrably occurred, and the HID interface said nothing about it. That was
the last scenario in which the interface could have been working.

**Conclusion, now unconditional: on firmware 113 the Argon PWR UPS publishes a complete and
valid USB HID Power Device descriptor and serves no data through it whatsoever.**

### Consequences

- The Argon-proprietary **serial protocol is the only UPS channel**. Task T4 is therefore not
  an alternative route to the UPS feature; it is the only one.
- Task **T8 is retired**. It asked whether the writable `RemainingCapacityLimit` survives a
  UPS power cycle, which presupposed being able to read it back. There is nothing to
  configure.
- The HID reader stays, gated behind actually receiving data, so a later firmware that
  implements the interface activates it with no further work. It cost little, and writing it
  produced an independent validation of the descriptor parser against the kernel's.
