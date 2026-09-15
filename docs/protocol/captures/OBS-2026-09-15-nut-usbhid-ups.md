---
project: argon-utils
doc: protocol/captures/OBS-2026-09-15-nut-usbhid-ups
status: evidence
last_updated: 2026-09-15
---

# OBS-2026-09-15-nut-usbhid-ups — can stock NUT drive the Argon PWR UPS?

Resolves `ARGON-UPS-HID-NUT`. **Answer: no, not with NUT 2.8.1 as shipped.**

This was M1's highest-value experiment because a positive result would have deleted a large
amount of planned work. It came back negative, for a more fundamental reason than expected.

## Method

Debian 13 `nut-server` 2.8.1-5. All NUT systemd units were stopped and disabled first so
nothing could claim the device on its own. The driver was run in the foreground with an
explicit device match, since the UPS enumerates with the generic Linux USB gadget IDs
`1d6b:0104` and is therefore in no subdriver table:

```
[argon]
    driver = usbhid-ups
    port = auto
    vendorid = 1d6b
    productid = 0104
    explore = yes
```

```
/lib/nut/usbhid-ups -DD -u root -a argon
```

## Result

The driver **matches** the device correctly:

```
[D2] - VendorID: 1d6b       - Manufacturer: Argon
[D2] - ProductID: 0104      - Product: Argon_USB
[D2] - Serial Number: XXXXXXXXXXXXXXXX
[D2] Device matches
[D2] successfully set kernel driver auto-detach flag
[D2] Claimed interface 0 successfully
[D2] Unable to get HID descriptor (Pipe error)
[D2] Unable to retrieve any HID descriptor
```

It then fails, because it looked for HID on the wrong interface. The UPS is a composite
device:

| Interface | Class | Kernel driver | Node |
|---|---|---|---|
| 0 | CDC-ACM control | `cdc_acm` | `/dev/ttyACM0` |
| 1 | CDC data | `cdc_acm` | — |
| **2** | **HID** | `usbhid` | `/dev/hidraw0` |

`usbhid-ups` looks for HID on **interface 0**. The `Pipe error` is correct behaviour: there
is genuinely no HID descriptor there.

## Why no amount of configuration fixes this

NUT 2.8.1's `usbhid-ups` has **no option to select a USB interface number**. Its full option
list offers only `usb_set_altinterface`, which sets `bAlternateSetting` — a different
concept entirely. The man page acknowledges the situation without providing a remedy:

> `usb_set_altinterface = bAlternateSetting` — Force redundant call to
> `usb_set_altinterface()`, especially if needed for devices serving multiple USB roles
> where the UPS is not represented by the interface number 0 (default).

So this is a driver limitation, not a configuration gap. Supporting this device in NUT would
require a patch adding interface selection — plausibly worth contributing upstream one day,
but out of scope here.

## Collateral damage, and the lesson

**The experiment broke the vendor daemon's serial port**, and this is the most important
thing in this document.

`usbhid-ups` sets libusb's kernel-driver auto-detach flag and claims interface 0. That
detached `cdc_acm`, `/dev/ttyACM0` disappeared, and the vendor's `argonupsrtcd` lost its
port. The driver did **not** restore the binding when it exited.

Recovery took an unbind/rebind of the whole USB device
(`/sys/bus/usb/drivers/usb/{unbind,bind}`) plus a restart of the vendor service. On the
first re-enumeration the node came back as **`/dev/ttyACM1`**, not `ttyACM0`, because the
old minor had not yet been released — which the vendor daemon could not follow, since it
hardcodes `/dev/ttyACM0`. A second re-enumeration with the daemon stopped reclaimed minor 0.

### Correction to a previously recorded claim

`docs/protocol/FACTS.md` and the project plan stated that the HID and CDC-ACM interfaces are
independent, so reading HID does not contend with the serial port.

That is **true for `hidraw` access and false for `libusb` access**, and the distinction is
not cosmetic:

| Access path | What it claims | Effect on the serial port |
|---|---|---|
| `/dev/hidraw0` | the existing `usbhid` binding on interface 2 | none |
| `libusb` (NUT, and anything using it) | a whole interface, **detaching the kernel driver** | breaks `/dev/ttyACM0` until re-enumerated |

**Rule for this project: use `hidraw`, never libusb, for UPS telemetry.** An interface claim
is not a read — it is an exclusive takeover that outlives the process that made it.

This also independently confirms the design premise that device nodes must never be
referenced by number: a single re-enumeration renamed `ttyACM0` to `ttyACM1`, and the vendor
software could not cope. `argon-utils` binds by `by-id` path.

## Consequences for the plan

- We write our own HID reader over `hidraw`. That work is **not** deleted.
- NUT is not available as a differential oracle without patching it. The HID decoder must be
  validated against the committed report descriptor and against the device's own serial
  channel instead.
- `nut-server` is left installed but with every unit disabled, in case we revisit it.
