---
project: argon-utils
doc: adr/0002
status: accepted
last_updated: 2026-09-15
---

# ADR-0002 — Default to the legacy MCU dialect, and never auto-probe

## Status

Accepted, 2026-09-15.

## Context

The Argon ONE-family MCU at I2C `0x1a` appears to speak two protocols:

- a **legacy** single-byte protocol, publicly documented by Argon40 — write `0x00`–`0x64`
  for fan duty as a literal percent, plus `0xFD`/`0xFE`/`0xFF`/`0xAA`/`0xBB` control bytes;
- a **register** protocol — `write_byte_data`/`read_byte_data` against registers `0x80`
  (duty), `0x82` (IR), `0x86` (control), whose existence we know only from reading the
  vendor's implementation, i.e. `inferred`.

The obvious design is to probe at startup and use whichever the device supports. The vendor
does exactly that: it reads register `0x80`, writes back the value plus one, and checks
whether the read-back changed.

## The problem with probing

An SMBus `read_byte_data(0x1a, 0x80)` is, on the wire:

```
S · addr+W · 0x80 · Sr · addr+R · data · P
```

The register number goes out **as a write** before the repeated start. Firmware that only
implements the legacy protocol has no concept of registers; it sees a byte, and `0x80` is
128, which clamps to a fan duty of 100%.

**A register read is therefore exactly as destructive as a register write.** There is no
non-destructive software probe for which dialect the MCU speaks. Any design that says
"safely detect the generation first" is mistaken, and the vendor's probe is the proof: it
audibly perturbs the fan for about two seconds at every single startup, and it is doing that
*because* it works by writing.

## Decision

1. **Default to legacy.** Resolution order is: explicit config → a confirmed entry in
   `identity.toml` → legacy. There is deliberately no `"auto"` value.
2. **Never probe implicitly.** Determining the dialect is a separate, explicit, interactive
   command that states what it will do to the fan and refuses to run in read-only mode.
3. **Gate register support at compile time.** The register code path lives behind a
   `unverified-register` Cargo feature that is off in shipped packages, so a stock install
   cannot emit a register transaction even if configured to.
4. **The only permitted discovery transaction is an SMBus quick-write** (address + W, zero
   data bytes). It transfers no data byte, so legacy firmware has nothing to misread.
5. **Refuse to act on an unrecognised device.** MCU writes require the live fingerprint to
   match `identity.toml`; on mismatch the daemon starts observe-only.

## Consequences

What legacy costs us, honestly:

| Lost | Mitigation |
|---|---|
| Reading back the current duty | Keep shadow state; we are the only writer anyway |
| Register `0x81` firmware version | Its semantics are `unknown`, so it is worthless either way |
| Register `0x86` power-off | Achievable with documented `0xFF` plus a logind poweroff |

That is the whole list. **Legacy is functionally complete for every deliverable this project
has**, so the safe default costs approximately nothing, while the unsafe default risks
physically misbehaving on hardware we cannot identify in advance.

## The register dialect ships, by configuration only, 2026-09-23

It was compiled out behind the `unverified-register` feature while it was `inferred`. Observed on
the ONE V3, it now ships -- and is reached **only** through `[mcu] dialect = "register"`. There is
still no `"auto"` and no probe; nothing about how a dialect is chosen has changed, only that a
second, observed one is available to choose.

Promoting it exposed a bug the gate had been hiding: the daemon and the CLI built every MCU with
`Dialect::default()`, ignoring the configured value entirely. While only legacy existed that was
harmless. With register available it would have meant a V3 configured correctly still being sent
legacy bytes. Every construction now takes the configured dialect, and a test asserts it.

## Both dialects observed, 2026-09-23

The register protocol this ADR held as `inferred` is real. On an Argon ONE V3 with a Pi 5, the
vendor's own daemon was watched on the bus through the kernel's I2C tracepoints -- the
clean-room route recommended below, without the logic analyser -- and its MCU returned from
register `0x80` exactly the duty it had just been given (`ONE-V3-MCU-REGISTER`).

So there are two dialects in the field, one per case: the ONE V1 is legacy, the ONE V3 is
register. The same register read that answers harmlessly on the V3 pinned the V1's fan at full.
That is this ADR's argument made concrete from both sides: there is no probe that is safe on both,
so the dialect stays configuration, never detection, and legacy stays the default because it is
the one that cannot damage either.

## Confirmed on hardware, 2026-09-22

The hazard this ADR is built around is no longer an argument from the protocol: on an Argon ONE
V1 with a Pi 4, a register **read** of `0x80` set the fan to full and left it there
(`ONE-V1-MCU-LEGACY`, `OBS-2026-09-22-t5-mcu-dialect`). A read, changing the fan, persistently.

A daemon that probed `0x80` at startup to "detect the generation" would pin that fan at full on
every boot. The decision below was taken before anyone had seen this happen; it is now the
observed behaviour of the only ONE-family MCU this project has been able to test.

To resolve the dialect for real, prefer a **logic analyser** on the I2C lines: watching what
the vendor's daemon transmits promotes the register facts from `inferred` to `observed` by
observing *the device* rather than by reading the vendor's code — which is also the
clean-room-correct path. Failing that, a one-off audible A/B test with the case within
earshot is definitive and instantly reversible.

## Alternatives rejected

- **Probe and restore.** Still writes; still perturbs the fan; still unsafe on firmware
  whose response to an unknown byte we cannot predict.
- **Probe only on first run.** Moves the hazard rather than removing it, and "first run"
  happens on every fresh image.
- **Register by default, fall back on error.** A legacy MCU does not *error* on a register
  write — it cheerfully sets the fan to 100%. There is no error to catch.
