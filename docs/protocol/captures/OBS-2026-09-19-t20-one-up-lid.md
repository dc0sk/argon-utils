---
project: argon-utils
doc: protocol/captures/OBS-2026-09-19-t20-one-up-lid
status: evidence
last_updated: 2026-09-19
---

# OBS-2026-09-19-t20-one-up-lid

Task T20, on the Argon ONE UP. **GPIO27 is the lid switch: high with the lid open, low with it
closed, one clean edge per movement.**

Full output: [`t20-2026-09-19-one-up-lid.log`](t20-2026-09-19-one-up-lid.log).

## Method

`~/t20-lid.sh`, run by the operator over SSH so the terminal stayed readable with the lid
shut. It stopped the vendor's `argononeupd` (which holds the line), read the level with
`gpioget -b pull-up`, watched both edges with `gpiomon -b pull-up -n 4`, read the level again,
and started `argononeupd` from an exit trap. The line was only read; the pull-up is the same
bias the vendor daemon requests. `argononeupd` was stopped from 08:36:01 to 08:36:18.

The operator closed the lid, opened it, and did so once more.

## Result

| Kernel time (s) | Edge | Interval |
|---|---|---|
| 45130.0356 | falling | lid closed |
| 45131.5960 | rising | open, after 1.56 s |
| 45134.4830 | falling | closed, after 2.89 s |
| 45135.3803 | rising | open, after 0.90 s |

The level read `active` (high) before and after, with the lid open.

- **Open = high, closed = low**, with the internal pull-up. That is what the plan inferred
  (`ONEUP-GPIO27` moves from `inferred` to `observed`).
- **No bounce.** One edge per movement at gpiomon's nanosecond timestamps. The switch is
  either a sensor with a clean output or debounced in hardware; either way, software needs
  no debounce beyond ignoring a flicker shorter than any human movement.

## What this does not tell us

- What `argononeupd` does when the lid closes. Its code is not read (clean room), and in this
  test it was stopped. Observing it means closing the lid with it running.
- Whether the lid does anything in hardware -- a display turned off by the panel itself, for
  instance -- independent of software.
