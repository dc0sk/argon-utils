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

## 🔴 T12 — Low-battery shutdown, end to end

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

**Everything is logged to `~/argon-t12/`.** If the machine does power off, nothing else
survives: `/tmp` is tmpfs, and Raspberry Pi OS sets the journal to `Storage=volatile`. The
home directory is on persistent storage.

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
