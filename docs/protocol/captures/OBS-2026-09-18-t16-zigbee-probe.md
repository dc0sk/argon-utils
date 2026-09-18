---
project: argon-utils
doc: protocol/captures/OBS-2026-09-18-t16-zigbee-probe
status: evidence
last_updated: 2026-09-18
---

# OBS-2026-09-18-t16-zigbee-probe

Task T16. **The Argon Industria Zigbee module answers Z-Stack MT at 115200 baud, and opening
its port as the probe does it does not restart the radio.**

Full output: [`t16-2026-09-18-zigbee-probe.log`](t16-2026-09-18-zigbee-probe.log).

## Method

`argonctl zigbee --probe`, run by the operator, with nothing else holding `/dev/ttyUSB0`. The
port was opened at 115200 8N1 without flow control; DTR was dropped inside `open()` and RTS
straight after, in that order. The probe then listened for 3 s sending nothing, and only then
sent `SYS_PING` and `SYS_VERSION`.

## Result

| Step | Sent | Received |
|---|---|---|
| listen 3 s | nothing | nothing |
| `SYS_PING` | `FE 00 21 01 20` | `FE 02 61 01 59 06 3D` |
| `SYS_VERSION` | `FE 00 21 02 23` | `FE 0A 61 02 02 01 02 07 01 6B B1 34 01 00 81` |

Both replies carry a valid FCS: `02^61^01^59^06 = 3D`, and the version frame's `81` likewise.

Decoded:

- **Ping:** capabilities `0x0659`. The bit meanings are not decoded here; the value is recorded
  raw.
- **Version:** transport revision 2, product 1, release 2.7.1, revision `6B B1 34 01`
  little-endian = **20230507**, a build date by its form.
- **One byte more than documented.** The version reply's length byte says 10, and the documented
  layout accounts for nine. The tenth is `0x00`; its meaning is unknown and it is recorded as
  such (`ZIGBEE-SYS-VERSION-TAIL`). The decoder reads the first nine and tolerates the rest.

## What it establishes, and what it does not

1. **The firmware speaks Z-Stack MT** (`ZIGBEE-FIRMWARE`), at the documented rate, with the
   documented framing -- promoting `ZIGBEE-BAUD`, `ZIGBEE-MT-FRAME`, `ZIGBEE-SYS-PING` and
   `ZIGBEE-SYS-VERSION` from `documented` to `observed` on this module.
2. **Opening the port this way does not restart the radio**: no `SYS_RESET_IND` in 3 s. This
   is the fact that matters in practice -- the probe is non-disruptive, and a health check can
   be run without knocking a coordinator over.

It does **not** establish that DTR/RTS are unwired. A reset line that is wired but received a
pulse too short to act on would look the same, and so would firmware that does not announce
restarts. The board's wiring stays `unknown`.

Which firmware build this is beyond its version fields is not claimed here.
