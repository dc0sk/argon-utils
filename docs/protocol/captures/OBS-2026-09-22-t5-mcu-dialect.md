---
project: argon-utils
doc: protocol/captures/OBS-2026-09-22-t5-mcu-dialect
status: evidence
last_updated: 2026-09-22
---

# OBS-2026-09-22-t5-mcu-dialect

Task T5, on an **Argon ONE V1** with a Raspberry Pi 4. **The device at `0x1a` speaks the legacy
single-byte protocol, and a register read of `0x80` set its fan to full -- the ADR-0002 hazard,
observed rather than argued.**

## Method

Three separate invocations, so the operator could hear each step and report before the next one
changed anything: `argonctl fan --t5 baseline|probe|restore --write`, printing the exact
transaction each time. The operator was in the room with the case throughout.

Nothing else drives that fan: no vendor software is installed, no `argon` process runs, and the
machine has **no kernel cooling device and no pwm hwmon** -- checked before the first write, so
a change in fan speed could only be ours. CPU 52.1 C at the start, 49.2 C at the end.

### Why a register read rather than the register write

T5 as first written said to send the register *write* and listen for the fan pinning at 100%.
That test is ambiguous: the register write is two bytes, `[0x80, duty]`, and legacy firmware
consuming both would read `0x80` as duty 128 and then `0x19` as 25%, **ending at the same duty
the register interpretation would produce**. The evidence would come down to hearing a
transient.

A register read puts exactly one byte on the bus before the repeated start, so its legacy
interpretation is unambiguous and *sustained*. It also returns a byte, giving a second,
independent signal.

## Result

| Step | On the wire | Fan | Returned |
|---|---|---|---|
| `baseline` | `i2c 0x1a <- 19` (legacy duty 25%) | **got louder** | -- |
| `probe` | `S 1a+W 80 Sr 1a+R <byte> P` | **full, and stayed full** | `0xc9` (201) |
| `restore` | `i2c 0x1a <- 32` (legacy duty 50%) | settled back | -- |

- **`ONE-V1-MCU-LEGACY`.** The fan went to full on a register *read* and stayed there. That is
  the legacy interpretation of `0x80` -- a duty of 128, clamped to full -- and nothing else
  explains a read changing the fan at all, let alone persistently.
- **The returned `0xc9` is not a duty.** Duties run 0-100; 201 is neither that nor the 25 we had
  just written. Register firmware answering `0x80` would have returned the stored duty. So both
  signals agree, and they are independent of each other.
- **Legacy commands work**: the documented single byte moved the fan twice, in both directions.

## What this settles

The register protocol (`ARGON-MCU-R-*`) stays **`inferred` and unconfirmed**, and now has
evidence *against* it on this unit. ADR-0002's default -- legacy, never auto-probed -- is
vindicated on real hardware: the "safe read-only probe" that the ADR argues does not exist,
demonstrably does not exist. A daemon that had probed `0x80` at startup would have pinned this
fan at full every boot.

## Not established

What `0xc9` is. It is whatever this firmware clocks out when asked to read, and a single byte
from a device that does not implement reads is not worth a theory.

Whether a V2, V3 or EON answers differently. Different cases, unmeasured; the dialect stays
configuration, never detection.
