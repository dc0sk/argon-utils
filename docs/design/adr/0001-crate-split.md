---
project: argon-utils
doc: adr/0001
status: accepted
last_updated: 2026-09-15
---

# ADR-0001 — Workspace split

## Status

Accepted, 2026-09-15.

## Decision

Split along two axes only — **does it perform I/O**, and **is it published** — rather than
by device type.

| Crate | Published | Rationale |
|---|---|---|
| `argon-proto` | yes | `no_std`, **zero I/O**. The only crate that can be fuzzed, property-tested and run under Miri with no hardware present |
| `argon-hal` | yes | Transports plus the `DryRun`/`ReadOnly`/`Locked`/`RateLimited`/`Recording`/`Replay` decorators |
| `argon-device` | yes | Capability composition, identity fingerprint, per-device drivers as *modules* |
| `argon-telemetry` | no | Prometheus + MQTT, as a **library** |
| `argond`, `argonctl`, `argon-tray` | no | Binaries |

## Consequences

**No crate per device.** `one`, `eon`, `ups`, `oled` are modules inside `argon-device`. Per-device
crates would multiply the version matrix for no isolation benefit — they all share one
capability vocabulary and would always be released together.

**The exporter is not a separate process.** This is the non-obvious one. A standalone
Prometheus exporter would need its own device access, which would make it a second reader on
the UPS serial port and a second I2C client — precisely the two-writer failure mode the
safety architecture exists to prevent. It links into the daemon and serves from state the
daemon already polls.

**`argon-proto` must never gain an I/O dependency.** That is the property the whole test
strategy rests on, so it is enforced by CI rather than by intent.
