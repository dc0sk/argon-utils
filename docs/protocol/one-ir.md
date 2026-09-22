---
project: argon-utils
doc: protocol/one-ir
status: specification
last_updated: 2026-09-22
---

# The Argon remote, and the ONE-family IR receiver

Measured from the remote and the case, not copied from anyone's table.

## Argon ONE V1 with a Raspberry Pi 4 -- `observed`

The receiver is on **BCM 23** and works with the stock `gpio-ir` overlay (`ONE-V1-IR-BCM23`).
The remote speaks **NEC**: a 9 ms mark and 4.5 ms space, 32 bits LSB first at 560 us, and
9 ms + 2.25 ms repeat frames while a key is held. **Address `0x00`** throughout.

### The keymap

| Button | Command | Button | Command |
|---|---|---|---|
| power | `0x9c` | menu | `0x9d` |
| up | `0xca` | back | `0x90` |
| down | `0xd2` | home | `0xcb` |
| left | `0x99` | vol+ | `0x80` |
| right | `0xc1` | vol- | `0x81` |
| OK | `0xce` | | |

Eleven buttons, three presses each, every frame passing both NEC inverse checks
(`ONE-V1-IR-KEYMAP`, `OBS-2026-09-22-t6b-one-v1-ir-keymap`). An earlier unlabelled capture
recorded the same set bar `0xce`, which is the cross-check that the labels are not merely
self-consistent.

### Pressing power over IR does nothing by itself

The machine carried on through the remaining ten buttons after power was pressed. So the case
does not act on the remote's power button in hardware, the way it acts on a held case button:
IR power is the host's to interpret, or to ignore.

## Why this is measured rather than borrowed

The vendor's IR table is all-rights-reserved, so we may not copy it -- and it is wrong in a way
worth recording: a missing comma concatenates two of its names, leaving ten names for eleven
codes, which their own issue tracker notes produces a wrong mapping. Our eleven names came from
the operator pressing eleven buttons in a stated order.

## Argon ONE V5 with a Pi 5 -- `observed`: no receiver

Four capture windows on BCM 23 and every free header line recorded nothing at all, with the
same overlay, tool and remote that read cleanly on the V1 (T6). The V5 is not wired for IR.

## Argon ONE V2, V3, EON -- **unmeasured**

Different cases. Whether they carry a receiver, and whether this remote's codes apply, is not
established here.
