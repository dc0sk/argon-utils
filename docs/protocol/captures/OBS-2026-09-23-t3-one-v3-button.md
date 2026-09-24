---
project: argon-utils
doc: protocol/captures/OBS-2026-09-23-t3-one-v3-button
status: evidence
last_updated: 2026-09-23
---

# OBS-2026-09-23-t3-one-v3-button

Task T3, on an **Argon ONE V3** with a Raspberry Pi 5. **A double-tap sends one 20.00 ms pulse on
GPIO4. A single tap sends nothing at all.**

Full output: [`t3-2026-09-23-one-v3-button.log`](t3-2026-09-23-one-v3-button.log).

## Method

With `argononed` retired (`mcu-takeover`), GPIO4 was unheld and nothing else acted on it -- no
`gpio-shutdown` overlay. Three kinds of watcher ran at once, each writing to a file on the V3 so
no pipe could hold output back:

- **GPIO**: `gpiomon` on every header line not muxed to a peripheral -- 4-13, 16, 17, 22-27.
  Deliberately excluded: 2/3 (the header I2C bus, which argond was using to drive the fan at the
  time; claiming them would have broken fan control), 14/15 (UART0, enabled), 18-21 (I2S for a
  DAC), 0/1 (the ID EEPROM bus). argond was confirmed still driving the fan during the sweep.
- **Input**: every `/dev/input/event*` device (13), for a key event -- in case the button was the
  Pi 5's own power key, as on the ONE V5 (T2).
- **UART0** (`/dev/ttyAMA0`, header pins 14/15), read-only at 115200, since `config.txt` enables
  it and an RP2040 reporting over serial was a plausible design.

The operator tapped in stated groups, and was asked whether they had been at the case before any
silence was taken as a result.

## Result

| Gesture | Presses | GPIO4 | Anything else |
|---|---|---|---|
| Single tap, ~3 s apart | 10, in two windows | **nothing** | nothing on any line, device or UART |
| Double-tap, ~3 s between pairs | 5 | **5 pulses** | nothing |

The five pulses, rising to falling:

| | Width |
|---|---|
| 1 | 20.002 ms |
| 2 | 19.998 ms |
| 3 | 19.999 ms |
| 4 | 19.999 ms |
| 5 | 19.999 ms |

Spaced 3.2-3.6 s apart, matching the pairs.

- **`ONE-V3-BTN-DOUBLE-TAP`: the V3 signals the host only for a double-tap**, with one pulse of
  **20.00 ms** (within 4 us across five) on GPIO4, active high.
- **`ONE-V3-BTN-SINGLE-SILENT`: a single tap sends nothing** -- no GPIO edge on any safe line, no
  input event, no UART byte. The firmware filters it, presumably so a brushed button does nothing.
- Not UART0, and not the Pi's power key: both were watched through every gesture and stayed silent.

## Against the ONE V1

| | ONE V1 (Pi 4) | ONE V3 (Pi 5) |
|---|---|---|
| Single tap | 1 pulse, ~20.09 ms | nothing |
| Double-tap | 1 pulse (the pair merges) | 1 pulse, 20.00 ms |

Same line, same polarity, the same ~20 ms width -- but on the V3 only a double-tap reaches the host.
Code that reads "a pulse on GPIO4" as one event works on both; code that expected a single tap to
do something would do nothing on a V3.

## A correction this settles

After the single taps alone came back silent, it was said here that retiring `argononed` had cost
the V3 nothing on the button, since a short tap does not signal the host. **That was wrong.** A
double-tap does reach GPIO4, which is the line `argononed` held for edges, so the takeover did give
up its handling of it. The caveat stated before the takeover was the accurate one.

## argond acting on it (0.1.39, same evening)

With `[button] action = "shutdown"`, argond claimed GPIO4 (`consumer="argond-button"`), and a
watcher on the V3 recorded argond's log and logind's `ScheduledShutdown` separately:

```
20:51:12  logind  nothing scheduled
20:52:30  argond  button: pressed; poweroff in 60 s -- press again to cancel
20:52:30  logind  "poweroff" at 20:53:30
20:52:32  argond  button: pressed again; poweroff cancelled
20:52:33  logind  nothing scheduled
```

A double-tap placed a real poweroff exactly a minute out; a second double-tap two seconds later
cancelled it; nothing was left pending. Two records kept apart -- the daemon's word and logind's
actual schedule -- agree.

**It first shipped unable to watch at all.** 0.1.38 logged "no header GPIO chip found" on this
machine: argond's sandbox admitted i2c, ttyACM and hidraw devices but not the GPIO chip, and the
device cgroup refuses an open before file permissions are consulted, so the `gpio` group did
nothing. Fixed in 0.1.39 (`DeviceAllow=char-gpiochip`), with a test tying the unit's allowlist to
every device class argond opens. Caught from the startup line, before any press.

**The announcement is weaker than intended on a desktop.** The tray shows a pending logind
shutdown in its menu and tooltip, with a cancel entry, but raises no notification, and
`shutdown`'s broadcast reaches terminals only. The tray was not running here at all -- the
session predated the install -- so the operator saw nothing.

**Resolved in 0.1.40.** The notification agent now also reads logind's pending shutdown and its
wall message. With the agent and tray started in the running desktop session, a double-tap raised
"Powering off at HH:MM: the case button was pressed. Press it again to cancel." and a second
double-tap raised "The scheduled poweroff was cancelled." Both were seen on screen by the operator
(2026-09-24), not only sent.

## Not established

- **Holds.** Not attempted: on this case a hold is the likeliest gesture for the RP2040 to act on
  itself, by cutting power.
- **What `argononed` did with a double-tap.** Only its bus traffic has been observed, never its
  behaviour on a pulse; finding out would mean letting it act on one, which may power the machine
  off.
