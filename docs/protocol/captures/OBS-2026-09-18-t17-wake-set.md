---
project: argon-utils
doc: protocol/captures/OBS-2026-09-18-t17-wake-set
status: evidence
last_updated: 2026-09-18
---

# OBS-2026-09-18-t17-wake-set

Task T17. **Command 6 sets the UPS wake schedule, from five BCD bytes `YY MM DD HH MM` in UTC,
and the device answers with an empty command-6 frame.** Promotes `ARGON-UPS-CMD6` from
`inferred` to `observed`, and adds `ARGON-UPS-CMD6-REPLY`.

Full output: [`t17-2026-09-18-wake-set.log`](t17-2026-09-18-wake-set.log).

## Method

`argonctl rtc --t17 --write`, run by the operator on mains with `argond` stopped. No schedule
was set beforehand (the tool refuses otherwise, since a schedule cannot be restored).

Both times were decades away on purpose. A Pi 5 set to power off on halt restarts when its
power returns, so the UPS very likely wakes it by cutting and restoring its output. If so, a
schedule coming due while the machine runs is an abrupt power cut. That mechanism is not
observed, and is assumed until it is (`ARGON-UPS-WAKE-MECHANISM`, `unknown`).

## Result

| Step | Sent | Received | Read back (x2) |
|---|---|---|---|
| baseline | queries only | -- | no schedule; UPS clock +1, +1, +0 s against the system |
| set 2098-07-13 06:29 | `FE 05 06 98 07 13 06 29 EA` | `FE 00 06 04` | 2098-07-13 06:29, exact, both reads |
| set 2097-03-21 17:42 | `FE 05 06 97 03 21 17 42 1D` | `FE 00 06 04` | 2097-03-21 17:42, exact, both reads |

What this establishes:

1. **Command 6 sets the wake schedule**, with the encoding and time base the ledger inferred.
   The two times differ in every field, so the second read-back cannot be a leftover of the
   first, and each matched to the minute.
2. **The reply is an empty command-6 frame**, `FE 00 06 04` (`FE+00+06 = 0x104`, low byte
   `04`), on both writes -- the same shape as the reply to a clock set in T15. Two set commands
   now share it (`ARGON-UPS-SET-ECHO`); it is not assumed for any other.
3. **Years up to 2098 are accepted**: BCD `98` stored and read back.
4. A **drift data point** from the baseline: the UPS clock read +0 to +1 s against the system
   clock at 13:52 UTC, 2 h 33 min after T15 set it -- within the method's resolution.

## What it does not show

- **How the wake works** -- whether it cuts and restores power, and so what happens if it comes
  due while the Pi is running. Deliberately not tested: the plausible answer is a power cut.
- **How to clear a schedule**, and whether one clears itself after firing. The schedule was left
  at **2097-03-21 17:42 UTC**, where it is inert.
- **That a wake actually powers the Pi on.** That needs the machine off, and is a separate test.
