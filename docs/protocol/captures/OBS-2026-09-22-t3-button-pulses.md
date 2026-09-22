---
project: argon-utils
doc: protocol/captures/OBS-2026-09-22-t3-button-pulses
status: evidence
last_updated: 2026-09-22
---

# OBS-2026-09-22-t3-button-pulses

Task T3, on an **Argon ONE V1** with a Raspberry Pi 4 Model B Rev 1.4. **The case signals the
host with one pulse of a fixed width -- 20.0-20.1 ms -- and nothing else. The vendor's three
pulse-width windows do not describe this case.**

## Method

`argonctl button`, which requests BCM 4 for edge events and times each pulse with the kernel's
nanosecond timestamps, and refuses to start if anything else holds the line. Nothing else on
that machine consumes BCM 4: no Argon software is installed, no `gpio-shutdown` overlay is
configured, and `gpioinfo` showed the line unheld. Read-only throughout; no I2C traffic.

Four runs, each a named gesture, with the operator at the case. The tool reports the gap before
each pulse, so a run can be read back as gestures rather than as a bag of widths.

## Result

| Gesture | Pulses | Widths (us) |
|---|---|---|
| 5 single short taps, ~3 s apart | **5**, one per tap | 20087, 20089, 20090, 20090, 20101 |
| 5 double-taps, ~3 s between pairs | **5**, one per *pair* | 20085, 20088, 20088, 20089, 20090 |
| 10 rapid taps | **5** | 20087, 20088, 20091, 20103, 20104 |
| 2 presses held ~2 s | **0** | -- |
| (an earlier mixed run) | 8 | 20085-20236 |

23 pulses in total, **20085-20236 us**, mean 20095. Excluding one outlier at 20236, the spread
is 20085-20104: 19 us across 22 events.

## What follows

- **`ONE-V1-BTN-PULSE`: the width is a constant, not a code.** A host cannot tell a single tap
  from a double-tap: both produce the same 20.09 ms pulse. Whatever the case encodes, it is not
  encoded here.
- **A hold never reaches the host.** Two 2-second holds produced no edge at all. Holding is the
  case cutting power in hardware, below anything Linux can see or veto -- consistent with the
  warning in [`../../testing/HUMAN-TASKS.md`](../../testing/HUMAN-TASKS.md).
- **Presses under about a second apart merge**: ten rapid taps gave five pulses, and a
  deliberate double-tap gives one. So the case debounces into a single event.
- **The vendor's windows (20-30 reboot, 40-50 shutdown, 60-70 display) are not measurable
  here.** Their lower window brackets what we measure, which fits a ~20 ms pulse timed by a
  `sleep(0.01)` accumulation loop; the other two windows correspond to nothing this case emits.
  They may still describe a V2 or V3, whose firmware we have not measured -- this is one case,
  and it is stated as one case.

## Not established

Whether a *longer* hold (3 s or more) emits a wider pulse before the power cut. Not attempted:
the cut is a hardware action on a machine that would lose its session, and the question does not
block anything. Whether anything answers I2C `0x1a` on this case is a separate question (T5),
and is not touched here.
