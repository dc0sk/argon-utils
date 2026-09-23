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

## Setting the clock on firmware 17 (T15, same day)

T15 was run against this unit to learn whether firmware 17 accepts the clock write that
firmware 113 does, rather than assuming it.

**First attempt, refused as designed.** In `mode = "read-only"` T15 read its baseline, found
the mode, and stopped without writing -- setting the clock is a full-mode operation, and the
experiment honours that like the daemon does. `argond`, restarted two seconds later, still read
an offset of **-580294198 s**.

**Second attempt, in full mode**, with every other precondition met (no wake schedule, system
clock NTP-synchronised):

```
13:51:58  read-only   clock offset -580294198 s     (18.4 years slow)
13:59:33  sudo argonctl rtc --t15 --write
13:59:43  full        clock offset +1 s, within tolerance
```

- **`ARGON-UPS-CMD3-FW17`: firmware 17 accepts the clock write.** The clock moved eighteen years
  to within a second of the system clock, which nothing but a `CMD3` write can do.
- **argond did not do it.** Its first reading after the restart says "within tolerance", not
  "set from the system clock"; the only thing between the two readings was T15, and it ran for
  the ~10 s the full experiment takes.
- **Not independently seen:** T15's printed exchange, including its negative control -- the
  deliberately wrong time (1 h 17 m 29 s behind) being read back before the restore. The end
  state proves the device takes the write; the read-back of the distinctive time would prove
  the sequence. Recorded at the tier the evidence supports.

## What this does *not* establish

**Wake scheduling on firmware 17.** T17 (setting a wake schedule) was run on firmware 113 only.
Setting the clock has now been seen to work on both firmwares; that is evidence about `CMD3`,
not about `CMD6`, and it is not stretched to cover it.

Whether its HID interface is dormant as firmware 113's is (`OBS-2026-09-15-ups-hid-is-dormant`).
Not attempted: the node is root-only until the package's udev rule is installed.

## The rest of the machine

- **The fan is the Pi's own**: `pwm-fan` bound as `cooling_device0`, kernel thermal governor
  driving it, as on the ONE V5 (T11). Not ours to take over.
- **The button is the Pi's own** (`pwr_button` on `PWR_GPIO`), as on the ONE V5 (T2).
- No header I2C bus (`dtparam=i2c_arm=on` not set); nothing on the machine needs it yet.
- No vendor software installed.
