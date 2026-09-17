---
project: argon-utils
doc: protocol/captures/OBS-2026-09-17-t12-low-battery-shutdown
status: evidence
last_updated: 2026-09-17
---

# OBS-2026-09-17-t12-low-battery-shutdown

Task T12. **The low-battery path works end to end on real hardware: losing mains is detected,
a delayed poweroff is scheduled with logind, and restoring mains cancels it. The machine was
never powered off.**

This is the first time the whole chain ran against the real UPS, the real `logind` and the
real desktop: UPS read -> policy -> shutdown coordinator -> `shutdown --poweroff +5` ->
cancellation. Everything before this was simulator or unit-test evidence.

## Method

`argond` in mode `full` with `docs/testing/t12-ups-shutdown.toml`: thresholds inverted
(`low_percent = 99`, `critical_percent = 98`) so a healthy battery counts as critical the
moment mains goes away, `confirmations = 2`, `shutdown_delay_min = 5`, 5 s poll. The vendor
UPS daemons were stopped for the duration. Mains was unplugged, then replugged well inside
the five minutes.

Two independent observers, neither of them `argond`:

- `docs/testing/t12-record.sh`, a per-second read-only timeline of logind's
  `ScheduledShutdown` property and of the status file -> `~/argon-t12/timeline.log`,
  committed for the two minutes around the event as
  [`t12-2026-09-17-timeline-excerpt.log`](t12-2026-09-17-timeline-excerpt.log).
- the daemon's own log -> `~/argon-t12/argond.log`, committed as
  [`t12-2026-09-17-argond.log`](t12-2026-09-17-argond.log).

The timeline matters because the daemon reporting its own success proves nothing. logind's
property is the machine's answer, not ours.

## Result

The daemon's view, in order:

```
argond: fan: no Argon MCU; the kernel's pwm-fan drives it, reported only
argond: mode full (configured full)
argond: ups: shutdown ENABLED: poweroff 5 min after the battery is confirmed critical, ...
argond: ups: unknown -> on mains at 92%
argond: ups: on mains -> battery low at 92%
argond: ups: battery low -> battery critical at 92%
argond: ups: poweroff SCHEDULED for SystemTime { tv_sec: 1789668727, ... }
argond: ups: battery critical -> on mains at 92%
argond: ups: poweroff cancelled
```

logind's own view, from the timeline:

| Second | `ScheduledShutdown` | Status file |
|---|---|---|
| 20:07:06 | `none` | `level=critical` (written before the schedule) |
| 20:07:07 | `"poweroff" 1789668727121393` | `shutdown_at=1789668727` |
| … 15 s … | unchanged | unchanged |
| 20:07:21 | `"poweroff" 1789668727121393` | `shutdown_at=1789668727` |
| 20:07:22 | `none` | `level=on-mains`, `shutdown_at=` empty |

`1789668727` is 20:12:07 local, exactly `shutdown_delay_min = 5` after the schedule was
placed at 20:07:07. Mains came back about 15 s in, and the poweroff was gone from logind the
same second the level changed. Nothing was left pending afterwards
(`ScheduledShutdown` = `(st) "" 18446744073709551615`).

Four things are confirmed by this, each of which was previously only asserted by a test:

1. **Losing mains is what triggers it, not the percentage.** The battery never left 92 %. The
   level moved because command 0's second byte changed, which is `ARGON-UPS-CMD0`'s
   `charging == 0 means mains` semantics working in the direction that matters.
2. **`shutdown --poweroff +N` works from a non-session process** owned by the user, with no
   polkit rule installed. The untested `polkit/50-argon-utils.rules` is for the packaged
   daemon running as the `argon` user; this run did not need it and did not use it.
3. **The cancellation path works and is prompt** -- one poll interval, not one shutdown delay.
4. **The status file is a faithful second source.** `shutdown_at` appeared and cleared in the
   same seconds as logind's property, so a consumer of the status file (the notification
   agent, the tray later) sees the truth without talking to logind.

## What this does not show

- **Whether the desktop notifications appeared** is not in these logs. `~/argon-t12/agent.log`
  is empty: the notification agent used in this run was the one already running from the
  earlier attempt, whose output went to a terminal that had been closed. The delivery path
  itself was separately confirmed by the user with `argonctl notify-agent --test`.
- **The real poweroff was never executed.** The five minutes were deliberately cut short. That
  `shutdown --poweroff +5` actually powers the machine off is systemd behaviour, not ours, but
  the last 300 seconds of this path remain unexercised here.
- **Nothing about a genuinely empty battery**: no discharge curve, no runtime-to-empty, and no
  evidence about how the UPS behaves below 20 %. The thresholds were inverted precisely to
  avoid needing that.

## Notes

The run also exposed a defect in the daemon's own logging, fixed alongside this capture:
the scheduled time was printed as a raw `SystemTime` debug struct, which is unreadable in a
journal. It now prints the remaining delay -- the number an operator actually needs, being
how long they have to restore mains -- and the unix time.

An earlier attempt the same evening failed twice before this one, for a reason worth keeping:
`argond` assumed an Argon MCU at I2C `0x1a`. It counted `argononed` as MCU contention and
degraded the whole daemon including UPS monitoring, then, once `argononed` was stopped, died
with `EREMOTEIO` on its first fan write. Neither had anything to do with the UPS. See
`ARGON-MCU-ABSENT-V5`; the daemon now probes for an MCU with an `SMBus` quick-write and never
lets the fan subsystem stop UPS monitoring.
