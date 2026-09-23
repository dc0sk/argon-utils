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

## Setting a wake schedule on firmware 17 (T17, same day)

T17 writes two different far-future wake times and reads each back, then leaves the second in
place: no command to clear a schedule is known, and none is guessed at.

```
13:59:43  argond      no wake schedule set
14:20:09  sudo argonctl rtc --t17 --write
14:20:16  argond      wake schedule 2097-03-21 17:42 UTC, far enough away to leave
```

- **`ARGON-UPS-CMD6-FW17`: firmware 17 accepts the wake-schedule write.** The schedule went from
  none to exactly T17's second target, and the reading is argond's -- a separate process with
  its own read of the device, not T17 checking its own work.
- **Not independently seen:** whether the *first* write (2098-07-13 06:29) took. The end state
  equals the second write, which is also what it would read if the first had been ignored.
  T17's printed read-backs would settle it.
- argond's safety net read the leftover schedule and judged it far enough away to leave, as
  designed.
- **The schedule stays at 2097-03-21 17:42.** That is 71 years out and will not fire. It does
  mean T15 and T17 refuse to run again on this unit -- both decline while a schedule is set --
  until a real wake clears it or `argonctl poweroff --wake-at` overwrites it.

## A wake firing on firmware 17 (T18, same day)

### First, removing a confound

The V5's T18 ran on a Pi 5 whose EEPROM had `POWER_OFF_ON_HALT=1` and `WAKE_ON_GPIO=0`. This Pi 5
had `POWER_OFF_ON_HALT=0`: halted, it stays in a low-power state with the PMIC on, waiting for its
button. A wake that failed there could not have been told apart from a Pi that never fully
powered down. So the EEPROM was matched to the V5 first, leaving the UPS firmware as the one
variable:

```
- POWER_OFF_ON_HALT=0
+ POWER_OFF_ON_HALT=1
+ WAKE_ON_GPIO=0
```

On this Pi 5, `rpi-eeprom-config --apply` writes to the flash's spare A/B slot and commits it
(`rpi-eeprom` 28.31, `AB_EEPROM`), so **nothing is staged in `/boot/firmware`** and the change
takes effect at the next boot. An empty boot directory is what success looks like; it is not a
sign the apply failed. (Two earlier reboots changed nothing because the original config had been
applied by mistake; the file names differed only after `eeprom-`.)

### The run

`sudo argonctl poweroff --wake-at "now + 20 minutes"` in full mode, on mains. A first attempt
with 10 minutes was refused by argonctl -- a wake must be at least 15 minutes out, so the machine
is certainly off before it comes due. A watcher off the machine recorded reachability:

| | |
|---|---|
| Went down | 14:51:19 |
| **Back up** | **15:11:15** (from `/proc/uptime`), reachable at 15:11:26 |
| argond's first wake check | **no wake schedule set** |

- **`ARGON-UPS-WAKE-FW17`: the wake fires on firmware 17.** The UPS powered the Pi on at the
  minute it was given, twenty minutes after the request, with the Pi's EEPROM matched to the V5.
- **`ARGON-UPS-WAKE-CLEAR-FW17`: the schedule clears itself after firing**, as on firmware 113.
  It also took the leftover T17 schedule with it, since T18 overwrote that first, so T15 and T17
  will run on this unit again.

### Why the boot time read 14:52, and the clock guard that caught it

`uptime -s` and the journal said the machine booted at **14:52:10**. That is the system clock at
boot, which resumes from roughly where it shut down and had not yet reached NTP -- not when the
machine came on. argond saw the discrepancy immediately:

```
clock offset +1146 s; not correcting it, because the system clock is not NTP-synchronised
```

The UPS clock had kept true time while the Pi was off, so it read 1146 s ahead. **14:52:10 +
1146 s = 15:11:16**, within a second of the real boot time from `/proc/uptime`: two independent
clocks agreeing on when the wake happened.

It is also the guard doing its job in the field. Copying that unsynchronised clock into the UPS
would have set it **19 minutes slow**, and every later wake would have come 19 minutes late. argond
refused, and once NTP had synchronised it looked again: `clock offset +1 s, within tolerance`.

## What this does *not* establish

What the UPS does if a wake comes due while the machine is running -- argond parks any schedule
that would, so it stays deliberately unobserved, as on firmware 113.

The EEPROM change remains in place on this Pi (`POWER_OFF_ON_HALT=1`, `WAKE_ON_GPIO=0`). Whether a
wake also works with the original `POWER_OFF_ON_HALT=0` is untested; the original config is kept
on the machine as `BACKUP-original-eeprom.conf`.

Whether its HID interface is dormant as firmware 113's is (`OBS-2026-09-15-ups-hid-is-dormant`).
Not attempted: the node is root-only until the package's udev rule is installed.

## The rest of the machine

- **The fan is the Pi's own**: `pwm-fan` bound as `cooling_device0`, kernel thermal governor
  driving it, as on the ONE V5 (T11). Not ours to take over.
- **The button is the Pi's own** (`pwr_button` on `PWR_GPIO`), as on the ONE V5 (T2).
- No header I2C bus (`dtparam=i2c_arm=on` not set); nothing on the machine needs it yet.
- No vendor software installed.
