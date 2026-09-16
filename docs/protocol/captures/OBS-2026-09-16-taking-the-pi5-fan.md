---
project: argon-utils
doc: protocol/captures/OBS-2026-09-16-taking-the-pi5-fan
status: evidence
last_updated: 2026-09-16
---

# OBS-2026-09-16-taking-the-pi5-fan — what manual fan control would actually cost

Follow-up to [OBS-2026-09-16-v5-fan-is-kernel-controlled](OBS-2026-09-16-v5-fan-is-kernel-controlled.md),
which established that the ONE V5's fan on a Pi 5 is driven by the kernel rather than by an
Argon MCU. This documents what taking it over would require, because the answer changes the
recommendation.

## The kernel's curve is already a curve

`thermal_zone0` (`cpu-thermal`), policy `step_wise`, five trip points:

| Trip | Temp | Type | Hysteresis |
|---|---|---|---|
| 0 | 110 °C | **critical** | 0 |
| 1 | 50 °C | active | 5 °C |
| 2 | 60 °C | active | 5 °C |
| 3 | 67.5 °C | active | 5 °C |
| 4 | 75 °C | active | 5 °C |

All five map to `cooling_device0` (`pwm-fan`, four states). So the kernel is running a
four-step curve at 50/60/67.5/75 °C with 5 °C hysteresis — which is not a placeholder, it is
a considered configuration, and close to what we would write ourselves.

## Taking over is all-or-nothing, and the price is the critical trip

There is **no runtime way to detach one cooling device from a zone**: `thermal_zone0` exposes
`cdev0..4`, their trip-point mappings and weights, but no `bind`/`unbind`.

Writing `pwm1` directly does not take control either — the governor rewrites it at the next
thermal update, so that is a fight rather than control.

The one mechanism that does work is the zone's `mode` attribute, which is writable and
currently `enabled`. Writing `disabled` stops the governor and leaves `pwm1` ours.

But the zone is the whole zone. **Disabling it also disables trip 0 — the 110 °C critical
trip**, which is the software thermal shutdown. Taking the fan therefore means taking
responsibility for thermal protection, not just for fan speed.

## What still protects the machine if the zone is disabled

Partially, and not identically:

```
$ vcgencmd get_throttled
throttled=0x0
```

The Pi's firmware throttles the SoC independently of Linux and reports it here, so thermal
runaway is not guarded *solely* by the Linux zone. But firmware throttling reduces clocks; it
is not the 110 °C emergency shutdown, and the two are not substitutes.

## Recommendation

**Report, do not control**, on Pi 5 hardware. Concretely:

- `KernelFan` is a **read-only** backend. It reports pwm, rpm and cooling state, and declines
  to write, with the reason in the error rather than in a comment nobody reads.
- The I2C `FanControl` backend remains the write path, for the Pi 4-era cases where the fan
  genuinely is ours to drive and no kernel governor is involved.
- If manual Pi 5 control is ever wanted, the honest route is a **device-tree overlay** that
  removes the cooling maps at boot — a deliberate, visible, reversible configuration change —
  not a daemon quietly writing `disabled` to a thermal zone at runtime.

The argument for taking over was a configurable curve and 256 PWM steps instead of four. The
argument against is that it costs the critical trip, and that the kernel's curve is already
reasonable. On this evidence the trade is not worth it, and the effort is better spent on the
UPS, OLED and button, which nothing else manages.

This is a recommendation, not a decision — see task T11.
