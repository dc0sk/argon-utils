---
project: argon-utils
doc: protocol/captures/OBS-2026-09-23-t5-one-v3-register-dialect
status: evidence
last_updated: 2026-09-23
---

# OBS-2026-09-23-t5-one-v3-register-dialect

Task T5, on an **Argon ONE V3** with a Raspberry Pi 5. **Its MCU speaks the register protocol:
register `0x80` holds the fan duty, and reads back the value just written to it.** Established by
watching the vendor's daemon on the bus, not by reading its code.

Full output: [`t5-2026-09-23-one-v3-i2c-trace.log`](t5-2026-09-23-one-v3-i2c-trace.log).

## Method

The vendor's `argononed` was installed and running on this machine, driving the case MCU. ADR-0002
names watching what that daemon transmits as the preferred way to settle the dialect: it observes
*the device*, not the vendor's code, and sends nothing of ours to the MCU.

Instead of a logic analyser, the kernel's own I2C tracepoints (`events/i2c/i2c_write`, `i2c_read`,
`i2c_reply`) recorded every transfer on the header bus while `argononed` was restarted, so its
startup traffic fell inside the window. Nothing in `/etc/argon` was read at any point. Only the
lines for address `0x1a` were kept.

(A first extraction came back empty: the filter looked for `addr=0x01a`, but the tracepoints print
the address as `a=01a`. The buffer still held the events, and re-extracting it with the right
pattern gave the lines below.)

## Result

Nine transfers from the vendor daemon, one second apart:

| t (s) | On the wire | Meaning |
|---|---|---|
| 372.328 | write `80`, read → `00` | read register `0x80`: duty 0 |
| 372.328 | write `80 01` | register `0x80` ← 1 |
| 373.329 | write `80`, read → **`01`** | read back: **1**, the value just written |
| 373.329 | write `80 00` | register `0x80` ← 0 |
| 374.330 | write `80 00` | the same, again |

- **`ONE-V3-MCU-REGISTER`: register `0x80` is the fan duty, and it is readable.** A device that
  returns exactly what it was just given is holding it in a register. There is no other reading of
  that third line.
- **The vendor probes by writing.** Read the register, write 1, read it back to confirm that
  registers work, then set the real duty -- 0 here, on a cool machine. The plan predicted a
  fan-perturbing startup probe; this is it, on the wire.
- **A register read changes nothing on this firmware.** The same transaction pinned the ONE V1's
  legacy fan at full (`ONE-V1-MCU-LEGACY`). Here it simply returned the stored duty.

## What follows

**The register protocol is real, and the legacy default is still right.** Two cases, two
dialects: the ONE V1 is legacy, the ONE V3 is register. A read that is harmless on the V3 pins the
V1's fan at full, so probing to tell them apart remains unsafe -- which is ADR-0002's whole
argument, now shown from both sides. The dialect stays configuration, never detection.

## Not established

- **Whether the V3 also accepts legacy single-byte writes.** The vendor daemon sent none, and
  sending one to find out would be a guess.
- Registers other than `0x80`. Only `0x80` appeared in the trace; `0x81`-`0x86` stay as they were.
- Whether other RP2040-based firmware revisions behave the same. This is one unit.
