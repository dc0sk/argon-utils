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

## 🟢 T2 — Does the case power button do anything?

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

**Result:** _(not yet done)_

---

## 🟡 T3 — Measure the real button pulse widths

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

## 🟡 T4 — Let us use the UPS serial port for an hour

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

**This is an approval, not a task** — say yes and it can be run unattended.

**Result:** _(not yet approved)_

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

## 🟡 T7 — OLED bring-up

**Unblocks:** the display feature. Your OLED module is installed but `enabled=N`.

**Good news:** the panel is confirmed present. A quick-write scan of `i2c-1` shows a device
at `0x3c`, which is the SSD1306. It is in fact the *only* thing on that bus besides the audio
DAC — there is no fan MCU.

Needs a photograph of the panel as evidence, since correct rendering cannot be verified in
CI. The init sequence will be written from the SSD1306 datasheet — the vendor's code never
performs a full init, so there is nothing there to copy even if we wanted to.

**Result:** _(not yet done)_

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
