---
project: argon-utils
doc: protocol/captures/OBS-2026-09-22-t6b-one-v1-ir-keymap
status: evidence
last_updated: 2026-09-22
---

# OBS-2026-09-22-t6b-one-v1-ir-keymap

T6 follow-up, on the **Argon ONE V1** with a Raspberry Pi 4. **Eleven remote buttons mapped to
eleven NEC commands at address `0x00`.**

Full output: [`t6b-2026-09-22-one-v1-ir-keymap.log`](t6b-2026-09-22-one-v1-ir-keymap.log).
Specification: [`../one-ir.md`](../one-ir.md).

## Method

`ir-ctl -r -d /dev/lirc0` for 180 s, with the `gpio-ir` overlay loaded at runtime on BCM 23.
The operator pressed **each button three times**, pausing between buttons, and reported the
order afterwards: power, up, down, left, right, OK, menu, back, home, vol+, vol-.

Decoding collapses consecutive identical codes into groups, so the grouping is derived from the
capture rather than assumed. Three presses per button makes a missed or extra press visible as
a group of the wrong size, which would break the alignment loudly instead of silently shifting
every label by one.

## Result

33 complete NEC frames, **11 groups of exactly 3**, every frame passing both inverse checks:

| # | Button (operator's order) | Address | Command |
|---|---|---|---|
| 1 | power | `0x00` | `0x9c` |
| 2 | up | `0x00` | `0xca` |
| 3 | down | `0x00` | `0xd2` |
| 4 | left | `0x00` | `0x99` |
| 5 | right | `0x00` | `0xc1` |
| 6 | OK | `0x00` | `0xce` |
| 7 | menu | `0x00` | `0x9d` |
| 8 | back | `0x00` | `0x90` |
| 9 | home | `0x00` | `0xcb` |
| 10 | vol+ | `0x00` | `0x80` |
| 11 | vol- | `0x00` | `0x81` |

**Cross-check:** the earlier unlabelled capture (`OBS-2026-09-22-t6-one-v1-ir`) recorded ten
codes, and they are exactly this set minus `0xce`. Two sessions, one of them blind to the
labels, agreeing on the alphabet.

**Incidental:** pressing power over IR did not power the machine off -- the capture continued
through the remaining ten buttons. The case acts on a held *case button* in hardware; it does
not act on the remote's power button.

## Not established

Whether the repeat frames should auto-repeat a key, which is a policy question rather than a
protocol one. Whether any other Argon case uses these codes.
