---
project: argon-utils
doc: protocol/captures/OBS-2026-09-18-t14-full-discharge
status: evidence
last_updated: 2026-09-18
---

# OBS-2026-09-18-t14-full-discharge

Task T14. **The packaged service powered the machine off on a genuinely low battery, as
configured, with nobody intervening.** Mains was unplugged at 88 % and left out; `argond`
running as the `argon` system user, outside any login session, under its polkit rule,
confirmed `critical` at 10 %, scheduled a poweroff two minutes out, and the machine went down
at the scheduled time. It was the first run of the whole chain in its deployed form.

## Why this run mattered

Every link had been verified separately: the coordinator against a fake logind, the chain in
T12 (but as the operator's own account, inside a desktop session where polkit needs no rule,
with inverted thresholds, and cancelled on purpose), and the polkit rule with `pkcheck` (T13
step 1, which asks the question without acting). The joint had never run: a non-session
system user actually getting a poweroff through logind, on a real low battery, to completion.

## Method

Packaged `argon-utils` 0.1.0 installed from the repo's `debian/` source package, `mode =
"full"`, shipped thresholds: `low_percent = 20`, `critical_percent = 10`, `confirmations = 2`,
`min_uptime_s = 120`, `shutdown_delay_min = 2`, UPS poll 10 s.

`docs/testing/t14-discharge-record.sh` sampled every 10 s into `~/argon-t14/timeline.log`,
reading only what `argond` publishes (`/run/argon-utils/ups.state`) plus logind's own
`ScheduledShutdown` property -- never the UPS serial port, which `argond` owns. The full log is
committed as [`t14-2026-09-18-timeline.log.gz`](t14-2026-09-18-timeline.log.gz) (verified to
decompress byte-identical to the original).

Load stayed at desktop idle, 0.2-1.1, and the SoC at 44-46 C, throughout.

## Result

| Time | Event | On battery |
|---|---|---|
| 07:59:07 | mains lost, 88 % | 0 |
| 10:21:35 | `low` at 20 % | 142 min |
| 10:36:59 | `critical` at 10 %, confirmed | 157 min |
| 10:36:59 | poweroff scheduled for 10:38:55, visible in logind | |
| 10:38:49 | last sample, 9 %, logind still `"poweroff" 1789720735103293` | 159 min 42 s |
| 10:38:55 | scheduled poweroff | |
| 10:38:59 | the next sample never happened | |

`argond`'s own lines, captured live by the watching session before the machine went down:

```
argond: ups: on battery -> battery low at 20%
argond: ups: battery low -> battery critical at 10%
argond: ups: poweroff SCHEDULED in 1m59s (unix time 1789720735)
```

**The machine was down within six seconds of the scheduled time**: the recorder, which
flushes every sample, wrote its last line at 10:38:49 and its next one was due at 10:38:59.

### Was it clean

The strongest direct evidence -- systemd's own shutdown log -- does not exist. Raspberry Pi OS
ships `/usr/lib/systemd/journald.conf.d/40-rpi-volatile-storage.conf` with `Storage=volatile`,
so the journal of the boot that powered off went with it. What the next boot shows:

- **No ext4 journal replay.** A filesystem that was not cleanly unmounted has its journal
  replayed at mount, and the kernel says so ("recovery complete"). That line is absent.
- One line, `EXT4-fs (nvme0n1p2): orphan cleanup on readonly fs`: inodes that were unlinked
  while still open when the root was remounted read-only at the end of shutdown. Whether any of
  those represents anything left unfinished cannot be settled from here; no fsck was triggered
  and nothing was reported lost.

So: consistent with a clean shutdown, by the evidence that survives. It is not the proof the
lost journal would have been.

### The discharge curve

Seconds per reported percentage point, in five-point bands, at constant load:

```
  85-89%   104s    60-64%   181s    35-39%   118s    10-14%    94s
  80-84%   100s    55-59%   153s    30-34%   106s     5- 9%    80s
  75-79%   114s    50-54%   152s    25-29%    98s
  70-74%   126s    45-49%   137s    20-24%    88s
  65-69%   150s    40-44%   122s    15-19%    88s
```

79 points in 159 min, mean 121 s. **The gauge never moved upwards** over those 79 points, which
matters because the policy acts on two consecutive readings.

The shape rises from ~100 s/point to a peak of 181 s around 60 % and falls back to ~85 s. At
constant draw, a percentage point that takes longer holds more energy, so the gauge's scale is
not linear in charge. That rise-and-fall is what a **voltage-derived** state of charge on a
lithium pack produces: steep at the top of the voltage curve, flat across the plateau. A
coulomb counter with a mis-set capacity would give a *uniform* error, so miscalibration alone
does not explain the shape -- though the operator's point stands that the pack is uncalibrated,
and the vendor recommends a full depletion to set its low-voltage reference.

**A prediction made during the run failed, and is recorded as such.** It was that the time per
point would collapse sharply below 20-25 % as the voltage knee arrived. It did not: the bottom
bands sit at 80-94 s, no faster than the top. A single 41-second point at 11 -> 10 % was read
at the time as the knee arriving; its band averaged 94 s, and one point is not a trend. The
knee, if there is a sharp one, lies below the gauge's 10 % -- which is where the gauge's own
calibration is least trustworthy.

### After the poweroff

The machine was powered on again at 10:51:54, 13 minutes later, **with mains connected** --
the operator's choice, because booting on a nearly empty pack under Pi 5 inrush is the one
genuinely risky moment in this exercise. `argond`'s first reading: `unknown -> on mains at
10%`.

The gauge read **9 % before the poweroff and 10 % after**. That is not a drain figure. A
voltage-derived gauge reads higher once the load is removed, because the cells recover at rest,
and charging on mains before the first reading adds some; this log cannot separate the two.

## What it establishes

1. **The deployed poweroff path works on a real low battery**, end to end, with the daemon
   running as a non-session system user. This is the first time the polkit rule was exercised
   by an actual shutdown rather than by `pkcheck`.
2. **Runtime at desktop idle: 2 h 37 min from 88 % to `critical`.** From a full charge it would
   be somewhat longer, and under load shorter; the number is for this machine at this load.
3. **The two-confirmation rule cost one poll at the critical threshold**: 10 % was first
   sampled at 10:36:49 with the level still `low`, and `critical` at the next sample, 10:36:59.
   (Both argond and the recorder run at 10 s, so this resolution is the sampling's, not the
   policy's.)
4. **The logging fix from T12 earned its place.** `poweroff SCHEDULED in 1m59s` is readable at
   the moment it matters; the `SystemTime { tv_sec: ... }` it replaced was not.

## What it does not show

- **Whether the desktop notifications appeared.** Not observed: the operator was away from
  the screen at both moments. They had been seen working in earlier runs, so this is a gap in
  this run's evidence rather than a known failure.
- **The halt-drain rate**, and so how long a full depletion for calibration would take with the
  machine off. The machine was off for 13 minutes, and the gauge rose rather than fell.
- **Watt-hours.** The pack estimate of roughly 15 Wh assumes a draw of 4.6 W that was never
  measured. An inline USB meter on the mains side would turn it into a number.
- **Anything below 9 %.** The shutdown did its job; the region below it is unobserved.

## Notes

The run also exposed an error in this repository's own documentation. On 2026-09-17 a review
concluded the journal was persistent, from `/var/log/journal` existing and `journald.conf`
leaving `Storage=auto` commented out; the T12 notes were edited to say so. Both observations
were true and neither decided it -- the Raspberry Pi OS drop-in overrides `journald.conf`, and
`systemd-analyze cat-config systemd/journald.conf` shows the effective `Storage=volatile`. The
original note had been right. It has been restored, with the drop-in named so the same
mistake is harder to make twice.
