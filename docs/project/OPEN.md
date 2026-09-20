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
| **B2** | IPC: D-Bus service and Unix socket | **built 2026-09-18**: the root-only socket, and `org.argonutils.Daemon1` on the system bus with polkit (logind's power-off defaults). Carries "power off with a wake". Verified on the real bus with 0.1.7 installed (2026-09-18): `Version` gives "0.1.7"; `CanPoweroffWithWake` gives "yes" from the active desktop session and "challenge" from a sessionless process (`systemd-run --uid=nobody`). `PoweroffWithWake` itself is exercised by T18 or the tray |
| ~~**B3**~~ | ~~OLED status page in `argond`~~ — **built 2026-09-18** | confirmed on the case panel: orientation, legibility at contrast 64, nothing clipped. Off by default; `[oled] enabled = true` |
| **B4** | UPS scheduled wake | **clock sync built 2026-09-18**: argond checks the UPS clock at startup and every 6 h and sets it when more than 2 s off (full mode, NTP-synced system clock, `sync_clock`). **Built 2026-09-18**: `argonctl poweroff --wake-at` (argond sets the wake, reads it back, then powers off) and a safety net that parks any schedule coming due on a running machine. **T18** is the real test, waiting for a run |
| ~~**B7**~~ | ~~Persistent drift record~~ — **built 2026-09-18**: every clock check appends to `/var/lib/argon-utils/clock.log` (bounded to 1,000 lines, cut to the newest 500), and `argonctl rtc` reports a rate once an uncorrected stretch spans a day. Earlier points, before the record existed: +0 s at 11:52 and +0/+1 s at 13:52 UTC on 2026-09-18, after T15 set the clock at 11:19 | — |
| **B5** | Zigbee detect/health | **working on hardware (T16, 2026-09-18)**: `argonctl zigbee` identifies the module; `--probe` reads the firmware version, non-disruptively. Firmware *update* tooling is not built, and would need its own risk decision |
| **B8** | ONE UP battery | **built 2026-09-18**: `[ups] source = "oneup"` reads the ONE UP's CW2217 fuel gauge (identified by VERSION `0xA0`; reads only, through a bus type with no write method) into the same policy, shutdown coordinator and status file as the PWR UPS. `argonctl battery` reads it by hand. Run on the ONE UP from a scratch folder in read-only mode: identified, published "on mains 100 %". **T19 step 1 passed 2026-09-19** (0.1.10, packaged, alongside `argononeupd`): charger out -> "on battery" at current -2256, back in -> "on mains", each within one poll. 0.1.11 installed there 2026-09-19; its tray reads "Battery 100 %" (confirmed on the ONE UP's panel). **Handed over 2026-09-19 (T19 step 2):** argond in mode full owns the battery, the lid overlay and lid agent the lid (T22: power-save and shutdown both work), `argononeupd` disabled -- `oneup-restore` reverts. **Open:** what else `argononeupd` did is unobserved; the keyboard light (T24); no wake on the ONE UP, so "Power off now…" is not offered there |
| **B6** | Tray controls | **started 2026-09-18**: "Power off now… and wake in 1 h / 8 h / tomorrow 07:00", offered only when polkit could allow it (confirmed in the panel with 0.1.7). **0.1.8**: any shutdown logind has scheduled is shown and can be cancelled in one click (confirmed 2026-09-18 with `shutdown -P +30`, cancelled from the tray); argond's low-battery poweroff keeps its two-step cancel. **0.1.23**: a "Case display" checkbox switches the OLED status page (D-Bus `OledState`/`SetOled`, polkit `org.argonutils.oled`), remembered across restarts; confirmed on the panel 2026-09-20 (T26) |

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
| ~~**D1**~~ | ~~`argon-proto` licence~~ — **decided 2026-09-19: `GPL-3.0-or-later OR MPL-2.0`** | — | **No**, once published |
| ~~**D2**~~ | ~~A settings deny rule on `/etc/argon/**`~~ — **decided 2026-09-19: no rule** | — | — |
| ~~**T11**~~ | ~~Fan on Pi 5: control it, or report it~~ — **decided 2026-09-19: report only** | — | — |
| ~~**S1**~~ | ~~Scope~~ — **decided 2026-09-19: tested hardware only** | — | — |
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



### D1 — the `argon-proto` licence — **DECIDED 2026-09-19: dual `GPL-3.0-or-later OR MPL-2.0`**

GPLv3 on a *library* would keep every permissively-licensed Rust project from depending on it.
The pure protocol crate is meant to be usable as the community's Argon protocol crate, so it is
dual-licensed: MPL-2.0 keeps file-level copyleft while allowing linking. The programs (`argond`,
`argonctl`, `argon-tray`) and the other crates stay GPL-3.0-or-later. Applied in the crate's
`Cargo.toml`, its SPDX headers, `LICENSE-GPL-3.0` and `LICENSE-MPL-2.0` beside it, and
`debian/copyright`. Still unpublished; publishing makes it irreversible.

### D2 — a settings deny rule on `/etc/argon/**` — **DECIDED 2026-09-19: no rule**

No technical block. The clean-room rules in `CLEANROOM.md` stand on their own; its claim that a
`.claude/settings.json` rule enforced them was untrue -- no such rule existed -- and was removed.

### T11 — fan ownership on Pi 5 — **DECIDED 2026-09-19: report only**

The kernel's `pwm-fan` governor keeps the fan and its 110 °C critical trip; argon-utils shows fan
and temperature in the tray, OLED and status. Evidence in
[OBS-2026-09-16-taking-the-pi5-fan](../protocol/captures/OBS-2026-09-16-taking-the-pi5-fan.md).
Fan *control* remains for cases with an Argon microcontroller (ONE V2/V3 with a Pi 4).

### S1 — scope — **DECIDED 2026-09-19: tested hardware only**

The project supports what can be tested here, and says so; nothing ships as blind support.

| Hardware | State |
|---|---|
| ONE V5 + Pi 5, PWR UPS, OLED, Zigbee module | supported, tested |
| ONE UP | battery and lid tested; lid actions and hand-over await T19/T22 |
| ONE V2 + Pi 4 | planned: in reach, untested (T3, T5) |
| EON, Fan HAT | planned only if hardware to test becomes available |
| NEO 5, NVMe boards, POLY+, THRML, Mini Fan, HMI displays, BLSTR | nothing to control |

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
