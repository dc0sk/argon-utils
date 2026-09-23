---
project: argon-utils
doc: protocol/captures/OBS-2026-09-23-neo5-industria-ups-5000
status: evidence
last_updated: 2026-09-23
---

# OBS-2026-09-23-neo5-industria-ups-5000

An **Argon NEO 5** on a Raspberry Pi 5 Model B Rev 1.0, with an **Industria UPS 5000**.
**The UPS answers the same serial protocol as the PWR UPS 10000 -- but reports firmware 17,
where the tested unit reports 113.**

## The UPS was invisible, and the cause was a config.txt filter

The UPS is wired to the case's internal USB header, as on the ONE V5. Nothing enumerated: no
`Argon_USB`, no `ttyACM`, and `dmesg` showed the keyboard as the only USB device since boot.

The cause was `config.txt`:

| Machine | `dtoverlay=dwc2,dr_mode=host` sits under | In force on a Pi 5? |
|---|---|---|
| ONE V5 (UPS working) | `[all]` | yes |
| NEO 5 (UPS invisible) | `[cm5]` | **no** -- a Pi 5 is not a CM5 |

Without `dwc2` in host mode the internal USB controller never comes up, so the header is dark
and the UPS has no bus to appear on. Adding the line under `[all]` and rebooting was the whole
fix. Nothing was wrong with the cable or the board.

`argonctl setup` reports exactly this ("the case's internal header stays dark without it"), and
its `config.txt` parser is filter-aware for this reason: a `[cm5]`-only line must not be read as
configuration on a Pi 5.

## What the UPS reports

Read with `argonctl ups --serial auto`, which goes through the query-only transport -- nothing
that changes the device can be sent from it:

```
firmware       17
clock          2008-05-04 00:23:50 UTC
wake schedule  none set
battery        59%, on mains
```

- **`ARGON-UPS-FW-17`.** The same frame format, the same commands, the same replies: battery
  status, firmware, clock and wake schedule all answered. So the protocol spans at least these
  two firmware versions and two capacities.
- **Its clock has never been set** -- 2008 is 18 years out. Plausible as a date, so argond's
  clock check treats it as a clock to correct rather than a garbled read, which is the intended
  behaviour for a clock reset by a deep discharge.
- 59% on mains at the time of reading.

## What this does *not* establish

**Nothing about writes on firmware 17.** T15 (setting the clock) and T17 (setting a wake
schedule) were run on firmware 113. Those facts stay tied to the firmware they were observed
on: a device that answers the same queries need not accept the same writes, and assuming it
does is how an `inferred` fact gets promoted without evidence. Running T15 and T17 against this
unit is what would settle it.

Whether its HID interface is dormant as firmware 113's is (`OBS-2026-09-15-ups-hid-is-dormant`).
Not attempted: the node is root-only until the package's udev rule is installed.

## The rest of the machine

- **The fan is the Pi's own**: `pwm-fan` bound as `cooling_device0`, kernel thermal governor
  driving it, as on the ONE V5 (T11). Not ours to take over.
- **The button is the Pi's own** (`pwr_button` on `PWR_GPIO`), as on the ONE V5 (T2).
- No header I2C bus (`dtparam=i2c_arm=on` not set); nothing on the machine needs it yet.
- No vendor software installed.
