# argon-utils

GPLv3 Rust tooling for Argon40 Raspberry Pi enclosures and the Argon PWR UPS — a clean-room
replacement for the vendor's Python stack.

> **Status: early development (M0).** Nothing here controls hardware yet. The protocol
> codecs and the project's safety scaffolding exist; the daemon, CLI and tray do not.
> Capability claims below state their evidence tier honestly — see
> [Evidence tiers](#evidence-tiers).

## Why

Argon40's official software works, but it has problems worth fixing: unbounded busy-poll
loops, `sleep()`-based GPIO timing that misclassifies button presses under load, a
UPS shutdown path that is edge-triggered on a notification *string* (so a missed transition
means no shutdown), no machine-readable output, no metrics — and no license at all.

`argon-utils` aims to replace it with something you can script, monitor, and trust around
your power supply.

## What it will support

Derived by crawling [github.com/Argon40Tech](https://github.com/Argon40Tech) and by
inspecting real hardware. Argon ships 14 SKUs; only five have any control software, and
several products are passive enclosures with no controllable electronics at all.

| Hardware | Fan | Button | OLED | IR | UPS | RTC |
|---|---|---|---|---|---|---|
| ONE V2 / V3 | ✔ | ✔ | — | ✔ | — | — |
| **ONE V5** | ✔ | ✔ | ✔ | ? | via PWR | via PWR |
| EON | ✔ | ✔ | ✔ | ✔ | — | ✔ |
| Fan HAT | ✔ | ✔ | — | — | — | — |
| ONE UP | — | ✔ | — | — | ✔ | — |
| **PWR UPS 5K/10K** | — | — | — | — | ✔ | ✔ |
| NEO 5 | — | — | — | — | — | — |

Not supported, because there is nothing to control: THRML, POLY, Industria HMI displays,
and "ONE M.2/NVMe" (which is two `config.txt` lines on any Pi 5, not a device).

The Industria **Zigbee** module is detected and health-reported only — it is a CC2652P
speaking standard TI Z-Stack, and [zigbee2mqtt](https://www.zigbee2mqtt.io/) already does
that job properly.

## Safety

This software can stop a fan, cut power, and shut down the host. The design reflects that:

- **Read-only is the default.** A fresh install cannot change your fan.
- **Dry-run is a transport decorator, not an `if`** — so no code path can bypass it by
  forgetting a flag.
- **The MCU dialect defaults to the documented legacy protocol and is never auto-probed.**
  There is no safe probe: an SMBus register *read* puts the register number on the bus as a
  write first, so reading register `0x80` on legacy firmware is indistinguishable from
  setting the fan to 128%. See [ADR-002](docs/design/adr/0002-legacy-mcu-dialect-by-default.md).
- **The fan fails loud, not silent** — a crashed daemon must never leave the fan stopped.
- We **never** emit MCU opcode `0xBB` (bootloader entry). The exit sequence is unknown and
  a failed flash can leave the MCU unrecoverable.

## Clean room

Argon40's implementation is all-rights-reserved (no LICENSE file), so this project may not
copy or translate it. Implementation is written against the specifications in
[`docs/protocol/`](docs/protocol/), each fact carrying a provenance status, and facts still
marked `inferred` may not back a write path. See [`CLEANROOM.md`](CLEANROOM.md).

## Evidence tiers

Every capability claim in this project states how it was verified. "CI is green" means
*simulator-validated*, which is not the same as *hardware-validated*, and neither is
reported as the other.

| Tier | Meaning |
|---|---|
| `documented` | From published vendor documentation or a component datasheet |
| `observed` | Measured on hardware we own, with the capture committed as evidence |
| `simulated` | Verified against a simulator or replayed tape only |
| `inferred` | Believed true, not yet verified — **may not back a write** |
| `untested-hardware` | Implemented from a datasheet, never run on the real device |

## Building

```sh
./scripts/gate.sh     # everything CI runs: fmt, clippy, tests, no_std, clean-room canary
```

The gate reports each command's real exit status. That is worth stating because a piped
`cargo clippy | grep error | head` reports the status of `head`, so a failing lint reads as a
clean run — a mistake made while building this, which is why the script exists.

> **Debian note.** The `rust-clippy` apt package installs `/usr/bin/cargo-clippy`, which
> shadows rustup's shim and will fail with `can't find crate for core`. Put `~/.cargo/bin`
> ahead of `/usr/bin` in `PATH`, or run `rustup run 1.95.0 cargo clippy`.

## License

GPL-3.0-or-later. See [COPYING](COPYING).
