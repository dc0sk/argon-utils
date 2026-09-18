---
project: argon-utils
doc: project/OPEN
status: living
last_updated: 2026-09-18
---

# Open items

Everything not yet settled, in one place. Hardware tasks live in
[`../testing/HUMAN-TASKS.md`](../testing/HUMAN-TASKS.md); this is the index and the
reasoning.

## Deferred

### Prometheus exporter — built, working, parked

**Status: complete and committed, deliberately not a priority.** Enabled with
`[telemetry] enabled = true`; off by default.

It serves CPU temperature, fan pwm/rpm, thermal governor state, vendor contention and device
presence, verified against a live scrape. It has no battery metrics, because nothing can
measure the battery yet.

Deferred 2026-09-17 at the user's direction. Nothing depends on it, and it does not decay:
the renderer is a pure function with tests, so it will still work whenever it matters. If the
battery channel opens up (task T4), adding those metrics is a small change.

**Do not spend further effort here** until the higher-value items below are settled.

## Not yet built

Deliverables from the plan that do not exist yet, so nothing claims them by omission.

| | What | Notes |
|---|---|---|
| ~~**B1**~~ | ~~Man pages~~ — **built 2026-09-18**: `argond(8)`, `argonctl(1)`, `argon-tray(1)`, generated from the clap definitions at package build | — |
| **B2** | IPC: D-Bus service and Unix socket | the tray and notification agent read the status file instead, which covers display; controls beyond `shutdown -c` need this |
| ~~**B3**~~ | ~~OLED status page in `argond`~~ — **built 2026-09-18** | confirmed on the case panel: orientation, legibility at contrast 64, nothing clipped. Off by default; `[oled] enabled = true` |
| **B4** | UPS scheduled wake | **clock sync built 2026-09-18**: argond checks the UPS clock at startup and every 6 h and sets it when more than 2 s off (full mode, NTP-synced system clock, `sync_clock`). Scheduled wake still needs command 6 confirmed the way T15 confirmed command 3 |
| **B7** | A persistent record of UPS clock drift | argond logs each check's offset, but the journal is volatile on Raspberry Pi OS, so the record ends at every reboot. First data point: +0 s at 2026-09-18 11:52 UTC, 2 h 33 min after T15 set it. Needs a small state file (and `StateDirectory=` back in the unit, now safe since the rollback record moved out of it) |
| **B5** | Zigbee detect/health | **working on hardware (T16, 2026-09-18)**: `argonctl zigbee` identifies the module; `--probe` reads the firmware version, non-disruptively. Firmware *update* tooling is not built, and would need its own risk decision |
| **B6** | Tray controls beyond cancelling a poweroff | waits on B2 |

Done since the plan: the tray icon (`argon-tray`, confirmed on the Pi desktop panel
2026-09-18: icon, tooltip and menu all render), the OLED status page (confirmed on the case
panel 2026-09-18), the notification agent, the Debian package.

## The question underneath the others

The project was planned around fan control. Two findings moved its centre of gravity:

- On the ONE V5 with a Pi 5, **the fan is not ours** — there is no MCU at `0x1a`, and the
  kernel thermal governor drives the fan competently through `pwm-fan`. Taking it over costs
  the 110 °C critical trip.
- The UPS's **HID interface is dormant** on firmware 113 — confirmed 2026-09-17 across a real
  mains-removal transition — so the Argon-proprietary serial protocol is the only channel
  that returns live data at all.

What is left that nothing else does, on the ONE V5 with a Pi 5: the **UPS** (battery state,
RTC, scheduled wake), the **OLED**, and the **Zigbee** module. On 2026-09-17 T2 showed that
the case button is the Pi's own power button, which the OS already handles, so it joins the
fan as something to leave alone on this hardware. IR is still unknown (T6). That is a coherent product, and arguably a more
useful one than a second fan controller. But it is a different product from the one the plan
opened with, and that is worth deciding rather than drifting into.

## Decisions

| | Decision | Blocks | Reversible? |
|---|---|---|---|
| **D1** | `argon-proto` licence: GPL-3.0-or-later, or dual with MPL-2.0 | publishing to crates.io | **No**, once published |
| **D2** | A settings deny rule on `/etc/argon/**` | nothing; reduces clean-room risk | yes |
| **T11** | Fan on Pi 5: control it, or report it | the shape of the fan feature | yes |
| **S1** | Scope: which hardware do we commit to? | what "supported" means in the README | yes |
| ~~**D3**~~ | ~~What to do when the battery policy advises shutdown~~ — **decided 2026-09-17** | — | — |

### D3 — acting on low-battery shutdown advice — **DECIDED 2026-09-17: implemented**

The user chose the recommendation: a delayed, cancellable shutdown, `full` mode only, with a
notification first. Implemented in `argond` with a desktop notification agent and a polkit
rule; verified on hardware in dry run, then end to end with a real scheduled poweroff on
2026-09-17 (task **T12**: scheduled, cancelled by restoring mains, nothing left pending --
`OBS-2026-09-17-t12-low-battery-shutdown`).

**Deployed 2026-09-17** as the `argon-utils` Debian package, armed in `mode = "full"`, with the
vendor's UPS daemons retired (and recorded for restoration on removal). **Verified in its
deployed form on 2026-09-18** by task T14: a real discharge from 88 % ended in the packaged
service powering the machine off at the configured delay, as a non-session system user under
its polkit rule (`OBS-2026-09-18-t14-full-discharge`).

<details><summary>The decision as it was put</summary>


UPS monitoring works on hardware as of 2026-09-17: `argonctl ups --serial auto --watch`
polls the battery and runs the battery policy, which is level-triggered, needs two
consecutive critical readings, never acts on a failed read, and holds off for two minutes after
boot. Defaults: low at 20%, critical at 10%.

**Nothing acts on its advice yet.** When it says shutdown, the tool prints `WOULD SHUT DOWN (not
enabled)`. Turning that into an action is a decision, because it is the one feature here that
powers the machine off:

- **How:** a clean `systemctl poweroff` through logind, or a delayed `shutdown +N` that can be
  cancelled if mains returns. The delayed form gives a window to cancel; the policy already
  cancels its own advice the moment mains returns.
- **Gating:** only in `full` mode, as the mode docs already say for power actions.
- **Where:** in `argond`, which means `argond` must own the UPS serial port — so the vendor's
  `argonupsrtcd` has to be retired on that machine, not merely stopped for a test.
- **Notice:** a desktop notification or wall message before acting.

My recommendation: a delayed, cancellable shutdown, `full` mode only, with a notification.

</details>



GPLv3 on a *library* prevents any permissively-licensed Rust project depending on it. If
`argon-proto` is meant to be the community's Argon protocol crate, that materially limits it.
Dual-licensing that one crate `GPL-3.0-or-later OR MPL-2.0` keeps file-level copyleft while
allowing linking; the binaries stay GPLv3 either way. **Irreversible once published**, so it
needs deciding before the first `cargo publish` — and not before.

### T11 — fan ownership on Pi 5

Evidence in
[OBS-2026-09-16-taking-the-pi5-fan](../protocol/captures/OBS-2026-09-16-taking-the-pi5-fan.md).
Current implementation is **report-only**, which is the recommendation. Changing it would
mean disabling the thermal zone, and with it the critical trip.

### S1 — scope

Hardware currently in reach, and what each would need:

| Hardware | State | Needs |
|---|---|---|
| ONE V5 + Pi 5 | primary target, working | nothing |
| PWR UPS | **serial protocol confirmed on hardware** | nothing — ready to build on |
| OLED | **working on hardware** (T7, 2026-09-17) | nothing — ready to build on |
| ONE V2 + Pi 4 | untouched | a session with that machine |
| Zigbee module | detected; health probe not built | a decision on how far to go |
| EON, ONE UP, NEO 5, Fan HAT | no hardware | would ship untested, or not at all |

The honest options are to support what can be tested and say so, or to ship blind support
marked `untested-hardware`. The project's evidence-tier discipline argues for the former.

## Hardware tasks

Detail in [`../testing/HUMAN-TASKS.md`](../testing/HUMAN-TASKS.md). Ranked by what they
unblock rather than by effort:

| | Task | Unblocks | Effort |
|---|---|---|---|
| **T3** | 30 button presses — **on the Pi 4 / ONE V2** | real pulse thresholds; the V5 has none | five minutes |
| **T6** | IR remote test | whether IR exists on the V5 at all | five minutes |
| **T9** | `0xFF` power-cut | power-cut arming | 🔴 deferred; Pi 4 first, never over SSH |

**T4 is done (2026-09-17) and the UPS is unblocked.** The serial protocol is confirmed on
hardware, its facts are `observed`, and wire captures are committed as test fixtures. What
remains for the UPS feature is implementation rather than discovery: a poll loop, low-battery
policy, the RTC and scheduled wake, and battery metrics for the (deferred) exporter.

T8 was retired alongside T1; it asked whether a writable HID threshold persists, which
presupposed being able to read it back.
