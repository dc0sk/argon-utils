---
project: argon-utils
doc: testing/HUMAN-TASKS
status: living
last_updated: 2026-09-15
---

# Tasks that need a human at the machine

> See [`../project/OPEN.md`](../project/OPEN.md) for the open decisions and for what each of
> these unblocks. The Prometheus exporter is **deferred** — built and working, but parked.

Everything here needs physical presence, a physical action, or a decision. Nothing else in
this project is blocked on them — each is listed with what it unblocks, so they can be done
in any order, whenever convenient.

**How to record a result:** append it to the task's "Result" line, and if it produces
evidence (a capture, a photo, a log) drop it in `docs/protocol/captures/` with a short
provenance note. A task whose result is "didn't work" is as valuable as one that succeeds —
several conclusions in this project came from negative results.

## Safety legend

> **Anything involving the power button can shut the machine down**, including the session
> you are running the test from. On Raspberry Pi OS the desktop handles the key itself, so a
> logind inhibitor gives no protection. Save your work first.


| | Meaning |
|---|---|
| 🟢 | Safe. Read-only or trivially reversible. Can be done any time. |
| 🟡 | Disturbs something briefly — a fan spins up, a daemon restarts. Reversible. |
| 🔴 | Can leave the machine off, unbootable, or needing physical intervention. **Never over SSH, never on the primary Pi first.** |

---

## ✅ T1 — Does the UPS emit a HID report on state change? — **DONE 2026-09-17: no**

> Mains was disconnected while the HID listener ran, with the vendor's serial daemon watched
> in parallel to confirm the transition really happened. Serial reported `Power:Battery 93%`
> immediately; HID emitted **nothing** across 60 s, and feature reads still returned only the
> echoed report ID. The HID interface is dormant unconditionally.
>
> **Consequence: the serial protocol is the only UPS channel, so T4 is now the only route to
> the UPS feature rather than one of two.**

<details><summary>Original task</summary>


**Unblocks:** whether the HID interface is truly dormant, or merely quiet while idle. This is
the last open question about `ARGON-UPS-HID-LIVE`.

Every probe so far was taken with the UPS sitting stable on mains at 93%. HID Power Devices
commonly report only on *change*, so the decisive test is to force one.

```sh
sudo ./target/debug/argonctl ups --wait 45   # sudo IS needed here: /dev/hidraw0 is root-only
# ...and while it is listening, unplug mains power from the UPS. Wait ~10s, plug back in.
```

**What it means**
- Reports appear → the interface works and is event-driven. Significant: we would prefer it
  for telemetry after all, and much of the serial work becomes optional.
- Nothing appears → the interface is decorative on firmware 113. Confirms current findings
  and we stay on the serial protocol.

</details>

**Result: no reports. HID confirmed dormant.**

---

## ✅ T2 — Does the case power button do anything? — **DONE 2026-09-17**

> **The case button is the Raspberry Pi 5's own power button.** No Argon MCU is involved. The
> desktop handles it: the first press opens a shutdown dialog, and a second press while the
> dialog is open runs `shutdown -h now`.
>
> **The test shut the machine down.** It had been set up with a logind inhibitor that was
> expected to prevent that, and it didn't, because on the Pi desktop labwc handles the key
> itself. Nothing was lost.
>
> So GPIO 4 being unheld is expected on this hardware, not a vendor bug, and `doctor` has
> been corrected. See
> [OBS-2026-09-17-v5-button-is-the-pi-button](../protocol/captures/OBS-2026-09-17-v5-button-is-the-pi-button.md).

<details><summary>Original task</summary>


**Unblocks:** whether we are fixing a vendor bug or matching vendor behaviour.

`argonctl doctor` reports that GPIO line 4 has no consumer and no process holds any gpiochip,
while the vendor's fan daemon runs happily with two file descriptors on `/dev/i2c-1`. Its
journal shows no error. That reads as: fan control works, button handling never started.

Press the case power button briefly (a short press — **not** a long hold, which cuts power in
hardware regardless of software).

**What it means**
- Nothing happens → confirmed vendor bug on Pi 5. A real feature for us.
- It reboots or shuts down → something is listening by a path we have not found, and the
  investigation needs reopening.

</details>

**Result: the Pi's own button, handled by the OS. Nothing for argon-utils to do here.**

---

## 🟡 T3 — Measure the real button pulse widths — **on the Pi 4 / ONE V2**

> **Changed 2026-09-17.** The ONE V5 has no MCU pulses to measure, because its button is the
> Pi's own power button (see T2). Only a Pi 4-era case can answer this.
>
> **Warning for any button test:** on the Pi desktop a logind inhibitor does *not* stop the
> power key. Save your work, or test from a text console.

**Unblocks:** replacing the vendor's pulse-width thresholds (which we hold as `inferred` and
do not trust) with our own `observed` specification.

The vendor's windows — roughly 20–30 ms reboot, 40–50 ms shutdown, 60–70 ms display-switch —
were derived from a `sleep(0.01)` accumulation loop that drifts under load. Ours will use
kernel nanosecond edge timestamps, which should be strictly better evidence.

```sh
./target/debug/argonctl button --count 30
```

No `sudo` needed — you are already in the `gpio` group.

Press the button ~30 times, mixing short and longer presses. The tool prints a histogram and
proposes thresholds with margins. It refuses to start if anything else holds the line, so it
cannot silently fight another process for it. If the vendor daemon is running it will not
interfere — it holds no GPIO line — but stopping it removes all doubt:

```sh
sudo systemctl stop argononed      # optional; `sudo systemctl start argononed` afterwards
```

**Result:** _(not yet done — produces `docs/protocol/one-button.md`)_

---

## ✅ T4 — Let us use the UPS serial port for an hour — **DONE 2026-09-17**

> Approved and run. The serial protocol works: framing, checksum and commands 0, 2, 4, 5 and
> 7 are now `observed`, with wire captures committed as test fixtures. Battery and firmware
> readings match the vendor daemon independently. The vendor daemon was stopped for the
> duration and restarted afterwards; read-only commands only.
>
> It found a real bug: with no wake schedule set the device answers with an **empty payload**,
> not five zero bytes, and our decoder reported that as a failure on a healthy device. Fixed.
>
> See [OBS-2026-09-17-ups-serial-capture](../protocol/captures/OBS-2026-09-17-ups-serial-capture.md).

<details><summary>Original task</summary>


**Unblocks:** promoting the entire UPS serial protocol from `inferred` to `observed`, and
recording replay tapes so the protocol has permanent regression tests that run in CI without
hardware.

The vendor's `argonupsrtcd` holds `/dev/ttyACM0` continuously, and CDC-ACM has no
arbitration — two readers corrupt each other. So this needs the vendor daemon stopped:

The tooling is ready, and refuses to run while anything else holds the port — verified
against the live vendor daemon:

```sh
sudo systemctl stop argonupsrtcd
sudo ./target/debug/argonctl ups --serial auto
sudo systemctl start argonupsrtcd
```

During the window the UPS is unmonitored, but it keeps working — the battery and charging are
hardware functions, and nothing we do changes them. Read-only commands only: battery status,
firmware version, RTC read, schedule read. No writes, and specifically no meter reset, which
would discard the battery meter's baseline.

The whole path is already tested against a simulated UPS over a real PTY, including
resynchronisation after line noise, rejection of corrupt frames, and deadline behaviour on a
silent or dribbling device. What hardware adds is confirmation that the *protocol* is what we
think it is.

</details>

**Result: the protocol works. Facts promoted to `observed`; tapes committed.**

---

## 🟡 T5 — Settle the MCU dialect, once — **on the Pi 4 / ONE V2**

> **Changed 2026-09-16.** This task originally targeted the ONE V5. It has moved, because
> there is **no MCU at `0x1a` on the V5 at all** — its fan is on the Pi 5's own header under
> kernel control. See
> [OBS-2026-09-16-v5-fan-is-kernel-controlled](../protocol/captures/OBS-2026-09-16-v5-fan-is-kernel-controlled.md).
> The Pi 4-era ONE V2 you own is now the only unit that can answer this.

**Unblocks:** whether an Argon MCU speaks the register protocol. Until settled we default to
the documented legacy protocol, which is functionally complete, so this is a nice-to-have
rather than a blocker.

**There is no safe software probe** — an SMBus register *read* puts the register number on the
bus as a write first, and legacy firmware reads `0x80` as a fan duty of 128. See
[ADR-0002](../design/adr/0002-legacy-mcu-dialect-by-default.md).

Two ways, in order of preference:

1. **Logic analyser** (if you have one — even a cheap 8-channel USB one). Clip onto GPIO 2/3
   (I2C SDA/SCL) and watch what the *vendor's own daemon* transmits. Zero writes from us, and
   it promotes the register facts to `observed` by observing the device rather than reading
   their code — which is also the clean-room-correct route.
2. **Audible A/B test.** With the case within earshot and the vendor daemon stopped, write a
   legacy duty of 25% and listen; then send the register form and listen. If the MCU is
   legacy-only, the second pins the fan at 100% — unmistakable, harmless, and instantly
   reversible.

**Result:** _(not yet done)_

---

## 🟡 T6 — Is IR actually wired on the ONE V5?

**Unblocks:** whether to implement IR receive for the V5 at all.

The vendor's V5 installer suppresses the IR menu, but the board reportedly has the
placeholders, and you own the remote. The receiver should be on BCM 23.

```sh
sudo ./target/debug/argonctl ir --watch   # not yet built -- will be, before this task matters
```

Point the Argon remote at the case and press buttons.

**Result:** _(not yet done)_

---

## ✅ T7 — OLED bring-up — **DONE 2026-09-17**

> The panel showed the test pattern correctly: normal orientation (no `--flip`), the full
> border, and no SH1106 shift. So it is a 128x64 SSD1306 at `0x3c`, and the init sequence
> written from the datasheet works. The evidence is the user's visual check; no photo was
> committed. See
> [OBS-2026-09-17-oled-bring-up](../protocol/captures/OBS-2026-09-17-oled-bring-up.md).

<details><summary>Original task</summary>


**Unblocks:** the display feature.

**What this touches, verified before writing these steps:** only I2C address `0x3c`. The
transport is bound to that address and refuses a write to anything else. It stops
`argononed` for the duration, which has no stop hook and, on a ONE V5 with a Pi 5, no hardware
to drive. **Nothing here involves power or the button, so it cannot shut the machine down.**

```sh
cd ~/git/argon-utils
./target/debug/argonctl oled              # optional: the pattern, drawn in the terminal
sudo systemctl stop argononed
./target/debug/argonctl oled --write      # no sudo: you are in the i2c group
#   ... look at the panel, take the photo ...
./target/debug/argonctl oled --off        # blank it; a static image burns an OLED in
sudo systemctl start argononed
```

**What you should see:** "argon-utils T7" and three more lines of text, a solid box in the
**top-left** corner, a one-pixel border round the whole edge, and a bar about 60% full.

| What the panel shows | What it means | Next |
|---|---|---|
| Exactly the above | Working | Photo, then `--off` |
| Upside down, box bottom-right | Mounted rotated | `--write --flip`, say which looked right |
| Text mirrored, box top-right | One axis flipped | Tell me; needs a single-axis remap |
| Shifted ~2 px, junk along one edge | An SH1106, not an SSD1306 | Tell me; different memory layout |
| Border missing on an edge | Not a 128x64 panel | Tell me what's cut off |
| Nothing at all | Init or wiring problem | Paste the command's output; check `argonctl doctor` still lists `0x3c` |

**Photo:** any quality that shows the text and the corner box. Drop it in
`docs/protocol/captures/` (for example `oled-t7.jpg`), or tell me where it is.

</details>

**Result: works. SSD1306 128x64 at `0x3c`, normal orientation.**

---

## ~~🔴 T8 — Does the UPS low-battery threshold survive a power cycle?~~ — **RETIRED**

> Retired 2026-09-17. The question presupposed being able to read the writable HID
> `RemainingCapacityLimit` back, and T1 established that the HID interface serves no data at
> all. There is nothing to configure.

<details><summary>Original task</summary>


**Unblocks:** whether a device-side low-battery threshold is a real safety guarantee or only
survives our daemon crashing.

HID report `0x11` (`RemainingCapacityLimit`) is writable but declared **Volatile**, so the
descriptor does not promise persistence. Moot while the HID interface is dormant (T1), but if
T1 shows the interface works, this becomes important.

Write a non-default value, fully depower the UPS, re-read. Rated 🔴 because fully depowering
a UPS means the Pi loses power unless it is on separate mains.

</details>

**Result: retired — the premise did not survive T1.**

---

## 🔴 T9 — The `0xFF` power-cut mechanism

**Unblocks:** arming the MCU's power-cut. Deferred well past current work; listed so it is
not forgotten and so the constraints are written down before anyone is tempted.

`0xFF` makes the MCU watch UART TX voltage and cut power when it drops. Combined with the
`POWER_OFF_ON_HALT=1` and `WAKE_ON_GPIO=0` already in your EEPROM, this could plausibly
produce a machine that halts and cannot be woken by the case button.

**Rules, not suggestions:**
- On the **Pi 4 / ONE V2** unit first. Never the primary Pi 5.
- Never over SSH. Keyboard, monitor, and physical access to the power supply.
- The recovery runbook gets written **before** the code, not after.

**Result:** _(deferred)_

---

## ✅ T12 — Low-battery shutdown, end to end — **DONE 2026-09-17: it works**

**Result.** Mains out -> `on mains -> battery low -> battery critical` -> poweroff scheduled
with logind for 5 minutes out; mains back in -> cancelled within one poll interval, with
nothing left pending. Confirmed against logind's own `ScheduledShutdown` property, not just
the daemon's log. Evidence and the four things it proves:
[`OBS-2026-09-17-t12-low-battery-shutdown`](../protocol/captures/OBS-2026-09-17-t12-low-battery-shutdown.md).

Two gaps remain, neither blocking: the desktop notifications were not captured in this run
(the agent's output went to a closed terminal; delivery itself was confirmed separately with
`argonctl notify-agent --test`), and the final poweroff was deliberately never allowed to
happen. The steps below are kept because they are the procedure for re-running it.

**Unblocks:** trusting the automatic shutdown with the real UPS, logind and the Pi desktop.

**This schedules a real poweroff.** Everything below is arranged so it is cancelled, but read
this first:

- **Save your work.** If the cancellation fails, the machine powers off cleanly 5 minutes
  after you unplug mains, and that ends any session running on it, including a Claude Code
  session.
- **Two ways to cancel:** plug mains back in, or run `shutdown -c`.
- **Do not stop `argond` before you see the cancellation.** A shutdown that is still pending
  when `argond` exits is deliberately left in place.
- **Run the terminals on the Pi's own desktop, not over SSH.** Polkit allows poweroff without
  a prompt only from an active local session; over SSH it would stop and ask for a password.
- logind will print a broadcast message in your open terminals. That's expected.
- **Not covered by the test:** the polkit rule. `argond` runs in your own desktop session here,
  where polkit allows poweroff without a rule. The packaged daemon runs as the `argon` user
  and needs `packaging/polkit/50-argon-utils.rules`; that part is untested until installed.

**Everything is logged to `~/argon-t12/`.** `/tmp` is tmpfs, so anything written there is
gone after a poweroff, and the home directory is on persistent storage.

The journal does not help either: Raspberry Pi OS ships
`/usr/lib/systemd/journald.conf.d/40-rpi-volatile-storage.conf` with `Storage=volatile`, so the
journal of the boot that powered off is gone -- `journalctl -b -1` answers "no persistent
journal was found". **The files in `$HOME` are the only record.**

(A 2026-09-17 edit claimed the opposite, reasoning from `/var/log/journal` existing and
`journald.conf` leaving `Storage=auto` commented. Both are true and neither decides it: the
drop-in overrides `journald.conf`, and `systemd-analyze cat-config systemd/journald.conf` shows
the effective value. T14 settled it -- the previous boot's journal was not there.)

The config in [`t12-ups-shutdown.toml`](t12-ups-shutdown.toml) uses absurd thresholds so any
reading on battery counts as critical, which triggers the path in seconds rather than hours.

### Steps

```sh
mkdir -p ~/argon-t12
sudo systemctl stop argonupsrtcd argononeupsd
```

**Terminal A** (notifications):
```sh
cd ~/git/argon-utils && ./target/debug/argonctl notify-agent --state /tmp/argon-t12.state 2>&1 | tee -a ~/argon-t12/agent.log
```

**Terminal B** (the daemon):
```sh
cd ~/git/argon-utils && ./target/debug/argond --config docs/testing/t12-ups-shutdown.toml 2>&1 | tee -a ~/argon-t12/argond.log
```

**Terminal C** (timeline recorder in the background; the terminal stays usable for `shutdown -c`):
```sh
cd ~/git/argon-utils && docs/testing/t12-record.sh &
```

1. Terminal B must show `fan: no Argon MCU; the kernel's pwm-fan drives it, reported only`,
   `ups: shutdown ENABLED` and `ups: unknown -> on mains`. Whether `argononed` runs no longer
   matters on the V5: there is no MCU for it to contend over. **If it says `shutdown in dry
   run`, stop**: a vendor UPS daemon is still running.

   *First attempt (2026-09-17):* argond warned about "another writer on the MCU", and after
   `argononed` was disabled it exited with `Remote I/O error`. Both came from assuming an MCU
   the ONE V5 does not have; the daemon now probes for one first and never lets the fan stop
   UPS monitoring. `argononed` was left **disabled** by that attempt, so it will not start at
   boot. That is harmless on this machine (the kernel drives the fan, the Pi drives the
   button); `sudo systemctl enable --now argononed` undoes it if wanted.
2. **Unplug mains.** Within about 10–15 seconds:
   - terminal B: `on mains -> battery low`, then `battery low -> battery critical`
   - terminal B: `poweroff SCHEDULED for …`
   - a **critical** notification: "Battery critical … powering off at HH:MM"
   - `shutdown --show` in terminal C: a pending poweroff about 5 minutes out
3. **Plug mains back in**, well within the 5 minutes:
   - terminal B: `battery critical -> on mains`, then `poweroff cancelled`
   - a notification: "Mains power restored … shutdown cancelled."
   - `shutdown --show`: nothing pending
4. **Only after nothing is pending:** Ctrl-C in A and B, `kill %1` in C, then:

```sh
sudo systemctl start argonupsrtcd argononeupsd
```

**Report:** which of the seven checks happened, and the contents of `~/argon-t12/`.

### If the machine shuts down

That happens either because the cancellation did not work, or because you let the 5 minutes
run out on purpose to see the real poweroff. Either way it produces useful evidence.

**Nothing needs restoring after it boots.** The vendor UPS daemons were stopped, not disabled,
so they start again at boot (`argononed` stays disabled, see step 1). A scheduled shutdown does not survive a reboot. The T12 status
file in `/tmp` is gone, which is expected.

Look at what was recorded:

```sh
tail -30 ~/argon-t12/argond.log                   # the daemon's view, up to the shutdown
grep -v 'shutdown=none' ~/argon-t12/timeline.log  # the seconds a shutdown was pending
tail -5  ~/argon-t12/timeline.log                 # the last seconds before power went off
```

Then start a new Claude Code session in `~/git/argon-utils` and say that **T12 ended with the
machine shutting down, logs in `~/argon-t12/`**, and whether mains was plugged back in before
it did.

**Result:** _(not yet done)_

---

## ✅ T14 — Full discharge: does the packaged service really power the machine off? — **DONE 2026-09-18: yes**

**Result.** Mains out at 88 % (07:59:07), `low` at 20 % after 142 min, `critical` at 10 %
after 157 min, poweroff scheduled for 10:38:55 -- and the machine was down within six seconds
of it, with the daemon running as the `argon` system user outside any session. **2 h 37 min
from 88 % to `critical`** at desktop idle. The gauge never moved upwards in 79 points. Evidence,
the curve, and what it does *not* show:
[`OBS-2026-09-18-t14-full-discharge`](../protocol/captures/OBS-2026-09-18-t14-full-discharge.md).

Notifications: not observed in this run -- the operator was away from the screen at both
moments -- but seen working in earlier runs. Still open: the halt-drain rate, since the machine
was off only 13 minutes before being rebooted on mains.
The steps below are kept as the procedure for re-running it.

**Unblocks:** the last unexercised link in the chain, and the one number nobody has: how long
this machine actually runs on the PWR UPS, and how long the fall from `low` to `critical`
takes.

**This test ends with the machine powering off. That is the point.** Unlike T12, nothing is
cancelled: mains stays out until `argond` decides, schedules, and lets the poweroff happen.

### Why this is worth the interruption

Everything about the packaged service has been verified *separately*: the poweroff path
against a fake logind, the whole chain in T12 (but as your user, in a desktop session, with
inverted thresholds and a cancellation), and the polkit rule with `pkcheck` (T13 step 1). The
joint has never run. This is also the only way to learn the real battery runtime -- the
thresholds (20 % / 10 %) were chosen on the principle that a fuel gauge is least trustworthy
near empty, not from measurement.

### Before you start

- **Save your work and close anything that matters.** The machine powers off on its own.
- Plan for it to take **hours**. From 90 % to 10 % at Pi 5 idle is unknown, which is the
  measurement; the recorder samples every 10 s so a long run is fine.
- `argond` must be in `mode = "full"` (`journalctl -u argond` should say
  `ups: shutdown ENABLED`). The recorder refuses to start otherwise.
- The recorder never opens the UPS serial port. `argond` owns it, and two readers on a
  CDC-ACM port desynchronise; it reads only the status file `argond` publishes.

### Run it

Start the recorder first if you can, but either order works:

```sh
cd ~/git/argon-utils
docs/testing/t14-discharge-record.sh start           # while still on mains
docs/testing/t14-discharge-record.sh start --anyway  # already unplugged
```

`--anyway` exists because unplugging first is the natural thing to do. It takes the true
mains-loss moment from argond's own journal line (`ups: on mains -> on battery at NN%`), so
every duration is still measured from the outage rather than from when the script started.

Then **unplug mains** and leave the machine alone. The recorder detaches with `setsid`, so it
survives the terminal closing, an SSH disconnect and the session ending.

```sh
docs/testing/t14-discharge-record.sh status    # whenever you want to look
```

Expect, in order: `on-mains` → `on-battery` → `low` at 20 % → `critical` at 10 % confirmed →
a scheduled poweroff → the machine goes off about 2 minutes later. Desktop notifications
should appear at `low` and at `critical`.

**To abort at any point:** plug mains back in. Cancellation now needs two consecutive mains
readings (~20 s), well inside the 2-minute delay. Harder abort:
`sudo shutdown -c && sudo systemctl stop argond`.

### The curve, and what it says about the gauge

```sh
docs/testing/t14-curve.sh          # per-point timings, five-point bands, pack estimate
```

Separate from the recorder on purpose: bash reads a script lazily, so editing the recorder
mid-run can change what the running shell executes next.

The shape is the interesting part. At constant load a well-calibrated coulomb counter gives
a flat column of seconds-per-point. A column that **rises and then falls** is the signature of
a **voltage-derived** state of charge on a lithium pack: steep at the top of the curve, flat
across the 3.8-3.7 V plateau, steep again past the knee. Attempt 2 showed exactly that rise
(100 s/point at 80-84 %, 181 s/point at 60-64 %) at flat load (0.2-1.1) and flat temperature
(44-46 C), so the machine's draw was not what changed.

This matters for a claim we should not make: **an uncalibrated pack is not what makes the
segments lengthen.** A wrong full-charge capacity is a *uniform* scale error, not a
progressive one. Calibration fixes where the endpoints land, not the shape in between. The
falsifiable half is whether seconds-per-point collapses below roughly 20-25 % as the knee
arrives; if it keeps lengthening to the end, the voltage-curve explanation is wrong.

The script also reports whether the gauge ever moved *upwards* while on battery. In attempt 2
it did not, over 37 consecutive points -- so single readings can be taken at face value for
direction, which is worth knowing given the policy only needs two confirmations.

### Depleting the rest, for the vendor's calibration

The vendor recommends a full depletion to give the gauge a low-voltage reference. That can be
had without risking a filesystem, because the two things are separable:

1. Let `argond` power the machine off at 10 %, as configured. The filesystem is down from
   here on, so nothing below can corrupt it.
2. **Leave it unplugged with the machine off.** The halted Pi (`POWER_OFF_ON_HALT=1`) plus the
   UPS's own electronics keep draining the pack down to the UPS's low-voltage cutoff, which is
   the calibration point.
3. **Boot with mains connected** and read the first percentage `argond` logs.

Step 3 is deliberate. Booting on a nearly empty pack is the single riskiest moment of the
exercise: boot is the most write-heavy minute a system has, a Pi 5 draws large inrush spikes,
and a pack at the knee is where the UPS can sag or trip under them -- an abrupt cut *during*
boot writes is the classic corruption case. Booting with mains costs under a tenth of a
percentage point of accuracy (charging is ~0.4 points per minute on a ~20 Wh pack, and
`argond` logs its first reading within a second or two), which is well inside the gauge's own
1 % resolution.

Do not lower `critical_percent` to chase a deeper *clean* shutdown: near-empty is where an
uncalibrated gauge is least trustworthy, which is the reason that threshold is 10 %.

Two caveats: **recharge promptly** rather than leaving the pack empty, and expect the UPS's
**RTC and wake schedule to be lost**, since they run off that same battery. The system clock
may therefore be wrong on the first boot, so note the wall-clock times yourself --
`t14-curve.sh` reconstructs the halt-drain interval from the log and the journal, and says so
when the clock is suspect.

### When it comes back

**Plug mains in before you power it on.** The battery will be at about 10 %, and booting on a
nearly empty battery is exactly the boot-loop case `min_uptime_s = 120` exists to blunt -- it
would give you roughly four minutes before a fresh poweroff.

```sh
cd ~/git/argon-utils
docs/testing/t14-discharge-record.sh report    # durations, discharge rate, did it power off
docs/testing/t14-curve.sh                      # the per-point curve
```

The previous boot's journal is **not** available: Raspberry Pi OS sets `Storage=volatile`
through a drop-in. The recorder's log in `~/argon-t14/` is the only record, which is why it
exists.

**Report:** the output of `report`, and whether the notifications appeared. The interesting
numbers are mains-loss → `low`, `low` → `critical`, and whether the poweroff happened at the
configured delay.

### Attempt 1, 2026-09-18: aborted after 220 s, by design

Mains out at 07:51:14 (89 %), back at 07:54:54 (87 %) — the operator replugged, so nothing was
scheduled and nothing powered off. Log kept as
`~/argon-t14/timeline-aborted-2026-09-18T0754.log`.

What it did establish:

- The recorder, the status file and the journal agree on the transition times, and the
  mains-back mark landed in the log correctly.
- **2 percentage points in 220 s**, i.e. roughly 110 s per point at desktop-idle load
  (load ~0.45, 44 °C). Taken at face value that would put 87 % → 10 % at about **2.4 hours**,
  but this is the least trustworthy part of the curve: the first points after mains loss are
  partly the gauge settling, not discharge. It is an order of magnitude, not a measurement —
  which is exactly why the full run is still worth doing.
- `argond` reported `on battery -> on mains` within one poll of replugging, and published
  `on-mains` again, so the recovery path works at this end of the curve too.

## ✅ T15 — Does command 3 really set the UPS clock? — **DONE 2026-09-18: yes**

**Result.** Confirmed: a distinctive wrong time read back within a second, then the correct
time was restored. The UPS answers a set with an empty command-3 frame, `FE 00 03 01`. The
clock had been 21-22 s slow. Evidence:
[`OBS-2026-09-18-t15-rtc-set`](../protocol/captures/OBS-2026-09-18-t15-rtc-set.md). The steps
below are kept as the procedure for re-running it.

**Unblocks:** B4 -- keeping the UPS clock right, and scheduled wake. It matters more than it
sounds: the UPS clock runs off the UPS battery, so a deep discharge like T14's can reset it.

**Why a test is needed at all.** Reading the clock (`ARGON-UPS-CMD5`) is `observed`. Setting
it (`ARGON-UPS-CMD3`) is only `inferred`, and the clean-room rule is that an inferred fact may
not back a write path until it has been seen to work. This task is that observation.

### What it does -- exactly

`argonctl rtc --t15 --write` sends **command 3 twice and nothing else that writes** (never 6,
the wake schedule, and never 9, the destructive meter reset):

1. Three baseline reads of the clock against the system clock, with nothing written.
2. Sets the UPS clock **1 h 17 min 29 s behind** -- a deliberately wrong, distinctive time --
   and reads it back twice.
3. Sets it to the **correct** time, timed to a second boundary, and reads that back twice.

Every byte sent and received is printed, including whatever the UPS answers to a set, which
nobody has recorded yet.

Why write a wrong time first: setting the clock to "now" proves nothing if it was already
close, because a read-back that matches is then indistinguishable from a write the UPS
ignored. A distinctive wrong time cannot be matched by accident.

**It refuses to start unless** the config says `mode = "full"`, no wake schedule is set (a
wrong clock could move or fire one), the system clock is NTP-synchronised (step 3 copies it),
and the three baseline reads agree with each other.

### Rehearsed before it reaches your hardware

Against the simulated UPS over a real PTY, three ways:

| Simulated UPS | Tool concludes |
|---|---|
| sets its clock | confirmed, restored |
| acknowledges the set but ignores it | **not** confirmed |
| ignores the set, and its clock was already correct | **not** confirmed, "restored" |

The third row is the case a naive "set it to now and read it back" test gets wrong. The
distinctive wrong time is what catches it.

### Risks, and why they are small

- The UPS clock is wrong for about five seconds. With no wake schedule set -- the tool checks
  -- nothing reads it in that window.
- `argond` has to be **stopped** for about half a minute, because the UPS port has exactly one
  owner. **Nothing watches the battery meanwhile, so do this on mains.**
- If the restore in step 3 fails, the output says so in capitals. Running the command again
  restores the clock.

### Run it

```sh
cd ~/git/argon-utils
cargo build -p argon-cli
sudo systemctl stop argond
sudo ./target/debug/argonctl rtc                                   # read-only look first
sudo ./target/debug/argonctl rtc --t15 --write | tee ~/argon-t15.log
sudo systemctl start argond
systemctl is-active argond                                         # must say: active
```

`sudo` because the packaged service gives the UPS port to group `argon`; `tee` runs as you,
so the log is yours.

**Report:** `~/argon-t15.log`. The line that matters is `T15 RESULT`; the `received` lines are
the other half -- what the UPS answers to a set is itself a new fact.

## ✅ T16 — Does the Zigbee module answer, and does opening its port restart it? — **DONE 2026-09-18**

**Result.** It answers: Z-Stack MT at 115200, release 2.7.1, revision 20230507. Opening the port
as the probe does it did **not** restart the radio -- which makes the probe safe to repeat,
though it does not prove the lines are unwired. Evidence:
[`OBS-2026-09-18-t16-zigbee-probe`](../protocol/captures/OBS-2026-09-18-t16-zigbee-probe.md).

**Unblocks:** B5, Zigbee health -- the scope you chose at the start: detect the module, report
its firmware, and say whether it is healthy.

**What is known and what is not.** The module is a CP2102N USB-serial bridge in front of a
CC2652P radio (`ZIGBEE-USB`, observed). What firmware the radio runs is not established; TI's
Z-Stack coordinator firmware is expected, and its serial protocol is published
(`documented`). What is **not** known, and has no datasheet: whether the bridge's DTR and RTS
lines are wired to the radio's reset and bootloader pins (`ZIGBEE-LINES`). On many CC2652
boards they are, and Linux raises both lines whenever a serial port is opened -- so on such a
board, merely opening the port restarts the radio.

### What the probe does -- exactly

`argonctl zigbee --probe`:

1. Opens the port at 115200 baud and **immediately lowers DTR, then RTS**. Opening raises both
   for a moment; that cannot be prevented from user space. Releasing the bootloader line
   before the reset line is the order in which, on the usual wiring, the radio restarts into
   its normal firmware rather than its bootloader.
2. **Listens for 3 seconds, sending nothing.** Z-Stack announces every restart with a
   `SYS_RESET_IND` message, so hearing one here is an observation of the wiring: opening the
   port restarted the radio.
3. Sends `SYS_PING`, then `SYS_VERSION` -- two read-only questions. Nothing that joins,
   forms, erases, writes or resets anything.

Every byte in both directions is printed. It refuses if anything else holds the port.

### Risks

- The radio may restart once when the port is opened. Nothing is using it, so nothing is
  interrupted.
- The worst case is a board wired in a way the release order does not suit, which could leave
  the radio **in its bootloader**. That erases nothing -- the bootloader only acts on commands,
  and none are sent -- but the radio would not answer until it is reset. A full shutdown and
  restart of the Pi will very probably reset it; that is not verified. If the probe reports
  NO ANSWER, tell me before trying anything else.
- It does not need `argond` stopped: `argond` does not touch the Zigbee module.

### Run it

```sh
cd ~/git/argon-utils
cargo build -p argon-cli
./target/debug/argonctl zigbee                              # identity only, opens nothing
./target/debug/argonctl zigbee --probe | tee ~/argon-t16.log
```

No `sudo`: the port is in group `dialout`, which your account is in.

**Report:** `~/argon-t16.log`. The `T16 RESULT` lines say whether the firmware answered and
whether opening the port restarted the radio; the `heard` line is the raw evidence for the
second.

## ✅ T17 — Does command 6 really set the wake schedule? — **DONE 2026-09-18: yes**

**Result.** Confirmed: two far-future times, each read back exactly; the UPS answers with an
empty command-6 frame. The schedule is left at 2097-03-21 17:42 UTC. Evidence:
[`OBS-2026-09-18-t17-wake-set`](../protocol/captures/OBS-2026-09-18-t17-wake-set.md). Still
unknown: how the wake works, and how to clear one.

**Unblocks:** B4, scheduled wake -- the UPS powering the Pi back on at a set time.

**Why a test is needed.** Reading the schedule (`ARGON-UPS-CMD7`) is `observed`; setting it
(`ARGON-UPS-CMD6`) is only `inferred`, so no wake write path may be built until it is seen to
work -- exactly as T15 did for the clock.

**Why the test times are decades away.** A Pi 5 set to power off on halt, as this one is,
restarts when its power comes back. So the UPS most likely wakes it by cutting and restoring
its output -- inferred, not observed, but it is the dangerous reading: a schedule that came
due while the machine was running would be an abrupt power cut. So T17 never writes a time
that could come due.

### What it does -- exactly

`argonctl rtc --t17 --write` sends **command 6 twice and nothing else that writes**:

1. Sets the wake schedule to **2098-07-13 06:29 UTC** and reads it back twice.
2. Sets it to **2097-03-21 17:42 UTC** -- different in every field, so the read-back cannot
   be a leftover of step 1 -- and reads it back twice.

Every byte sent and received is printed, including whatever the UPS answers to a set.

**It refuses unless** the config says `mode = "full"` and **no wake schedule is set now**: it
will not overwrite one of yours, because there is no known way to put it back.

**It leaves the schedule set, at 2097.** No command to clear a schedule is documented, and
guessing one -- an empty payload, say -- risks the UPS taking leftover bytes as a time. A
schedule 70 years out is inert; `argonctl rtc` and the report will show it.

### Rehearsed

Against the simulated UPS: a UPS that stores the schedule is confirmed; one that acknowledges
the write but ignores it is **not**. Separately, the two times are checked at compile time --
changing either to a near year does not build.

### Run it

Same shape as T15, on mains, because `argond` owns the UPS port and must be stopped for
the few seconds this takes:

```sh
cd ~/git/argon-utils
cargo build -p argon-cli
sudo systemctl stop argond
sudo ./target/debug/argonctl rtc                                   # read-only look first
sudo ./target/debug/argonctl rtc --t17 --write | tee ~/argon-t17.log
sudo systemctl start argond
systemctl is-active argond                                         # must say: active
```

**Report:** `~/argon-t17.log`. The `T17 RESULT` line is the verdict; the `received` lines
record what the UPS answers to a wake set.

## 🔴 T18 — Does the UPS actually wake the Pi at the scheduled time?

**Unblocks:** trusting `argonctl poweroff --wake-at`, and two unknowns: whether a wake works at
all (`ARGON-UPS-WAKE-MECHANISM`), and whether the UPS clears a schedule after it fires
(`ARGON-UPS-WAKE-CLEAR`).

**This powers the machine off**, for about 20 minutes, and ends any session on it. Save your
work first, and do it **on mains**.

### What happens

1. `argond` sets the wake about 20 minutes out, reads it back from the UPS, then schedules a
   poweroff one minute later -- announced to logged-in users, cancellable with
   `sudo shutdown -c`.
2. The machine powers off.
3. At the wake time the UPS should power it on again.
4. On boot, `argond` checks the schedule straight away. Its log then answers the second
   question: **"no wake schedule set"** means the UPS cleared the schedule when it fired;
   **"... was Past on a running machine; the wake has been parked"** means it did not, and
   `argond` moved it out of the way.

Nothing in this test lets a schedule come due while the machine runs: the wake is always at
least 15 minutes out when set, the poweroff comes first, and if the poweroff were cancelled
`argond` would park the wake before it came due.

### Run it

First install 0.1.6 (your config is kept) and check `argond` is listening:

```sh
sudo apt install -y -o Dpkg::Options::=--force-confold ~/git/argon-utils_0.1.7_arm64.deb
journalctl -u argond -n 20 --no-pager -o cat | grep -E 'control|wake'
```

Expect `control: listening on /run/argon-utils/control.sock` and the parked 2097 schedule from
T17 reported as far enough away to leave. Then:

```sh
sudo argonctl poweroff --wake-at "now + 20 minutes" --dry-run      # look first
sudo argonctl poweroff --wake-at "now + 20 minutes" | tee ~/argon-t18.log
```

It powers off a minute later. **Note the wake time it printed.**

### When it comes back

```sh
{ echo "booted: $(uptime -s)"; journalctl -u argond -b --no-pager -o cat | grep -iE 'wake'; } | tee -a ~/argon-t18.log
```

**If it has not come back ten minutes after the wake time**, press the case's power button
(the Pi's own; it starts a halted Pi 5) and report that the wake did not happen.

**Report:** `~/argon-t18.log`.

## 🟡 T13 — Does the polkit rule work for the packaged daemon? — **step 1 DONE 2026-09-17: yes**

**Unblocks:** trusting the low-battery poweroff when `argond` runs as the `argon` system user
rather than inside your desktop session.

T12 did **not** cover this. It ran `argond` as your own user inside an active login session,
where polkit allows a poweroff with no rule at all. The packaged service runs as `argon`,
outside any session, and needs `packaging/polkit/50-argon-utils.rules`. Which actions are
involved was verified with `pkaction --verbose` (scheduling with other sessions present checks
`power-off-multiple-sessions`; with none, `power-off`); what is untested is whether the rule
matches for a system-bus caller that is not in a session.

### Step 1 — ask polkit, without scheduling anything — **done: both authorised**

```sh
sudo -u argon sh -c 'pkcheck --action-id org.freedesktop.login1.power-off --process $$; \
    echo "power-off exit: $?"; \
    pkcheck --action-id org.freedesktop.login1.power-off-multiple-sessions --process $$; \
    echo "multiple-sessions exit: $?"'
```

Both returned `0` (authorised) on 2026-09-17 with the package installed, so the rule does
match for a system-bus caller outside any session. This asks the question without arming
anything, so it is safe to run at any time.

Note the shape of the command: `pkcheck` must run **as** `argon` and name **its own** process.
Running `sudo -u argon pkcheck --process $$` names the calling shell, which belongs to your
user, and polkit refuses that with "Only trusted callers ... can use CheckAuthorization() for
subjects belonging to other identities" -- a permission error about the *question*, not an
answer about the rule.

### Step 2 — the real thing, cheaply

Only worth doing if step 1 says yes and you want the end-to-end proof:

```sh
sudo -u argon shutdown --poweroff +60 "argon-utils polkit test"
busctl get-property org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager ScheduledShutdown     # expect: poweroff, ~60 min out
sudo -u argon shutdown -c                                # the cancel path, as the argon user
busctl get-property org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager ScheduledShutdown     # expect: "" 18446744073709551615
```

**If the last command still shows a poweroff, cancel it as root: `sudo shutdown -c`.** An hour
is deliberately long so there is no time pressure.

### Step 3 — the whole chain, packaged

Repeat T12 against the installed service instead of a hand-started binary. Two differences
matter: the config must be under `/etc/argon-utils/` (the unit sets `ProtectHome=yes`, so a
config in `$HOME` is invisible to it), and the daemon must be in `mode = "full"`.

**Report:** the two exit statuses from step 1, and whether step 2's cancellation worked as the
`argon` user.

## 📋 T11 — A new decision: should we control the fan at all on this machine?

Raised by the finding above. On your Pi 5 the fan is driven by the kernel thermal governor
through `pwm-fan`, bound to the CPU thermal zone with active trip points at 50, 60 and
67.5 °C and a critical one at 110 °C. It works, it is well tested, and it is not ours.

Taking it over is therefore a different proposition from taking over from the vendor's
daemon. It would mean displacing a working kernel controller, and the honest question is what
we would offer in exchange:

- **A configurable curve.** The kernel's trip points are set in the device tree. Ours are a
  config file you can edit without rebuilding an overlay.
- **Quieter, or louder-but-cooler, than the kernel's four steps.** `pwm-fan` gives four
  cooling states; direct PWM control gives 256.
- **One place for everything.** Fan, UPS, OLED and button in one daemon with one config, one
  metrics endpoint, one status command.

Against it: the governor already handles the thermal emergency correctly, and anything we
write has to be at least as trustworthy at 110 °C.

There is a middle option worth considering — **report but do not control**: expose the fan's
state in status and metrics, leave the governor in charge, and put our control effort into
the UPS, OLED and button, which nothing else manages.

**Your call, and not urgent.** Nothing is blocked on it.

**Result:** _(open)_

---

## 📋 T10 — Two decisions

**D1 — `argon-proto` licence.** GPLv3 on a *library* blocks every permissively-licensed Rust
project from depending on it. If this should become the community's Argon protocol crate,
dual-licensing that one crate `GPL-3.0-or-later OR MPL-2.0` keeps file-level copyleft while
allowing linking. Binaries stay GPLv3 either way. **Irreversible once published to
crates.io**, so it needs deciding before the first `cargo publish` — and not before.

**D2 — the `/etc/argon/**` deny rule.** The vendor's all-rights-reserved Python is on this
machine and is one `cat` from contaminating the clean-room claim. A repo-local settings deny
rule on `Read(/etc/argon/**)` and `Bash(cat /etc/argon/*)` would stop an assistant session
pulling it into context by accident. Yours to add — I have not touched your config.

**Result:** _(both open)_
