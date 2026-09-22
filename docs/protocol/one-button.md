---
project: argon-utils
doc: protocol/one-button
status: specification
last_updated: 2026-09-22
---

# The ONE-family case button

What the case does when someone presses its button, and what the host can know about it. One
case has been measured; the rest are marked as unmeasured rather than assumed.

## Argon ONE V1 with a Raspberry Pi 4 -- `observed`

The case signals the host on **BCM 4** with a pulse of **20085-20104 us** (`ONE-V1-BTN-PULSE`,
23 events, T3). That is the whole vocabulary:

| Gesture | What the host sees |
|---|---|
| Single short tap | one pulse, ~20.09 ms |
| Double-tap | one pulse, ~20.09 ms -- **the same**; the pair merges |
| Presses under ~1 s apart | merged into one pulse |
| Press held ~2 s | **nothing**; the case cuts power itself (`ONE-V1-BTN-HOLD-SILENT`) |

**The width carries no action.** A reader of this line can know that *a press happened*, and
nothing more. Software that switches on pulse width -- as the vendor's does, with windows at
20-30 ms, 40-50 ms and 60-70 ms -- has, on this case, one window that always matches and two
that never do.

So `argon-utils` treats a pulse on BCM 4 as a single event on this hardware, and does not
pretend to distinguish reboot from shutdown from display-switch where the hardware does not.

## Argon ONE V2, V3 -- **unmeasured**

Different boards and different firmware: the V3 is a Pi 5 case, and carries an RP2040. Whether
either emits more than one pulse width is unknown here, and inheriting the V1's answer would be
a guess. The vendor's windows may well describe them.

Until one is measured, no pulse-width thresholds ship for these cases.

## Argon ONE V5 with a Pi 5 -- `observed`, and not ours

The case button *is* the Pi's own power button: it arrives as `KEY_POWER` from `pwr_button`
with no MCU involved (`ARGON-BTN-V5-IS-PI`, T2). Nothing to decode, and nothing to take over.

## How it was measured

`argonctl button` requests the line for edge events and times pulses with kernel nanosecond
timestamps, rather than accumulating `sleep()` calls -- which is what makes 19 us of spread
across 22 events legible instead of being buried in the measurement's own jitter.
