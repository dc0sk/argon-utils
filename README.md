# argon-utils

GPLv3 Rust tooling for Argon40 Raspberry Pi enclosures and the Argon PWR UPS — a clean-room
replacement for the vendor's Python stack.

> **Status: working on two machines.** A Raspberry Pi 5 in an Argon ONE V5, with the Argon PWR
> UPS, the OLED and the Industria Zigbee module, runs it daily as a Debian package; an Argon
> ONE UP (the CM5 laptop) runs it alongside the vendor's daemon. Everything below says how it
> was verified -- see [Evidence tiers](#evidence-tiers). Other Argon hardware is not
> supported yet.

## What works today

On a Pi 5 in an ONE V5 with the PWR UPS -- all `observed` on that hardware unless stated:

| Feature | What it does | Verified |
|---|---|---|
| **UPS monitoring** | `argond` reads charge and mains state over the UPS's USB serial port and publishes it; `argonctl ups` prints it | daily use since 2026-09-17 |
| **Low-battery shutdown** | a delayed, cancellable poweroff when the battery reaches critical, with desktop notifications first; cancelled by itself if mains returns | a real discharge from 88 % to a clean poweroff (T14) |
| **UPS clock** | kept in step with the system clock, and its drift recorded across reboots | T15 |
| **Power off and wake** | `argonctl poweroff --wake-at 07:00`, or the tray: the UPS powers the machine on again at the set time | wake set and read back (T17); **the wake itself is not yet observed** (T18) |
| **Tray icon** | charge, CPU temperature and fan in the panel; shows and cancels a scheduled shutdown; "power off and wake" where polkit allows | in the Raspberry Pi desktop's panel |
| **OLED** | a status page on the case display | on the case panel |
| **Zigbee** | `argonctl zigbee --probe` identifies the module and reads its firmware version, without disturbing it | T16 |
| **Fan** | reported, not controlled: on a Pi 5 the kernel already drives it, and taking over would cost its critical trip | -- |

The ONE V5 has no fan microcontroller when fitted with a Pi 5, and its button is the Pi's own
power button, which the OS already handles. So on this machine the useful product is the UPS,
the OLED and the Zigbee module.

### Argon ONE UP

On the CM5 laptop -- `observed` on one unit:

| Feature | What it does | Verified |
|---|---|---|
| **Battery** | with `[ups] source = "oneup"`, `argond` reads the laptop's Cellwise CW2217 fuel gauge (reads only) into the same policy, poweroff, tray and notifications; `argonctl battery` prints it | identified and read on the unit; charger out and back in followed within a poll (T19 step 1) |
| **Lid** | a device-tree overlay makes the lid a standard lid switch; `argonctl lid-agent` then saves power while it is closed, or alerts and powers off, per `[lid]` | loaded at runtime, `logind` followed the lid (T20, T21) |
| **Handover** | `oneup-takeover` moves battery and lid from the vendor's `argononeupd` to these, recorded; `oneup-restore` undoes it exactly | against a fake root in the gate; **not yet run on the unit** |

No wake on the ONE UP: it has no UPS clock. Its fan and power key are the Pi's, as on the ONE V5.

Install from the Debian package: see [`packaging/README.md`](packaging/README.md). Nothing is
armed on install -- `mode = "read-only"` until you change it in `/etc/argon-utils/config.toml`.

## Why

Argon40's official software works, but it has problems worth fixing: unbounded busy-poll
loops, `sleep()`-based GPIO timing that misclassifies button presses under load, a
UPS shutdown path that is edge-triggered on a notification *string* (so a missed transition
means no shutdown), no machine-readable output, no metrics — and no license at all.

`argon-utils` aims to replace it with something you can script, monitor, and trust around
your power supply.

## Other Argon hardware

Apart from the ONE UP above, none of this is supported yet: the table is what the hardware can
do, not what this project does with it. Derived by crawling [github.com/Argon40Tech](https://github.com/Argon40Tech) and by
inspecting real hardware. Argon ships 14 SKUs; only five have any control software, and
several products are passive enclosures with no controllable electronics at all.

| Hardware | Fan | Button | OLED | IR | UPS | RTC |
|---|---|---|---|---|---|---|
| ONE V2 / V3 | ✔ | ✔ | — | ✔ | — | — |
| **ONE V5** | ✔\* | ✔\* | ✔ | ? | via PWR | via PWR |
| EON | ✔ | ✔ | ✔ | ✔ | — | ✔ |
| Fan HAT | ✔ | ✔ | — | — | — | — |
| ONE UP | — | — | — | — | ✔ | — |
| **PWR UPS 5K/10K** | — | — | — | — | ✔ | ✔ |
| NEO 5 | — | — | — | — | — | — |

\* With a Pi 4. With a Pi 5 there is no fan microcontroller, and the button is the Pi's own
(see above).

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
  (On a Pi 5 in the ONE V5 the question does not arise: the fan stays the kernel's.)
- **Power actions go through logind and polkit**, need `mode = "full"`, and are announced
  and cancellable. A wake that would come due on a running machine -- where the UPS's likely
  response is to cut power -- is moved out of the way before it can.
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
