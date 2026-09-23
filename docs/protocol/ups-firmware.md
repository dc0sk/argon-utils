---
project: argon-utils
doc: protocol/ups-firmware
status: open question
last_updated: 2026-09-23
---

# Updating UPS firmware: unknown, and deliberately not attempted

Two units are in hand and they run different firmware: an **Industria UPS 5000 reporting
firmware 17**, and a **PWR UPS 10000 reporting 113** (`ARGON-UPS-FW-17`). So the question
arises naturally: how is that firmware updated, and should `argon-utils` do it?

**We do not know, and we do not intend to implement it.** This page records what was checked,
what it rules out, and what evidence would answer it.

## What the devices expose

Read from sysfs on both units, 2026-09-23. They are descriptor-identical:

| | Value |
|---|---|
| VID:PID | `1d6b:0104` |
| `bDeviceClass` | `ef` (miscellaneous, interface-association) |
| Interfaces | CDC-ACM `02/02/00`, CDC-Data `0a/00/00`, HID `03/00/00` |
| **DFU interface** (`fe/01`) | **absent on both** |

- **No USB DFU path is advertised.** DFU is how a device says it may be flashed, and neither
  unit offers one, so no standard tool can address them.
- **`1d6b:0104` is the Linux Foundation composite gadget ID.** The UPS presents a Linux USB
  gadget stack rather than a bare microcontroller enumerating itself, which suggests updating
  happens *on* the device -- an updater in the vendor's image, or replacing storage -- rather
  than through a host-driven USB protocol.
- **Nothing in the serial protocol updates firmware.** Commands 0-9 cover battery status,
  charge current, the clock, the wake schedule and the battery-meter reset
  (`docs/protocol/FACTS.md`). No documented or observed command does anything else.

## What this does not rule out

A device can expose DFU **only after a trigger**: a command, a jumper, a button held at power
on. The honest statement is "no update path is exposed in normal operation", not "no update
path exists".

Finding such a trigger by probing is exactly what this project refuses to do. It would mean
sending undocumented commands to the device that supplies the machine's power, on the chance
of provoking a bootloader whose exit is unknown -- the same reasoning that keeps the case MCU's
`0xBB` documented and never emitted (ADR-0002, `docs/design/adr/0002`).

## Why no update mechanism will be built

1. **The failure mode is the power supply.** A failed flash does not break a feature, it
   potentially bricks the thing holding the machine up, possibly mid-write with the battery as
   the only power path.
2. **There is no known recovery.** No documented bootloader, no exit sequence, and no second
   channel.
3. **It cannot be tested where it matters.** Two units, no spares, and the failure path is the
   one that would need testing. Shipping a flasher whose failure path has never been exercised
   would contradict how everything else here was verified.

## What would answer the question

**Capture the USB traffic while the vendor's own updater runs.** That is observation of the
device, not of anyone's source, so it is clean-room legitimate -- the same route ADR-0002
prefers for the MCU dialect (a logic analyser over reading code). `usbmon` plus `tshark` on the
host running the updater would record the whole exchange.

Until someone does that, the facts stay as they are: firmware versions are **read and
reported**, and version-specific observations stay tied to the version they were made on. T15
(setting the clock) and T17 (setting a wake schedule) were first observed on firmware 113.
T15 and T17 have since been run on firmware 17 too: the clock write (`ARGON-UPS-CMD3-FW17`) and
the wake-schedule write (`ARGON-UPS-CMD6-FW17`) both work there. A wake actually *firing* (T18)
has not been tried on firmware 17 and is not claimed.
