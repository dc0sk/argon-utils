---
project: argon-utils
doc: protocol/captures/OBS-2026-09-15-ups-hid-descriptor
status: evidence
last_updated: 2026-09-15
---

# OBS-2026-09-15-ups-hid-descriptor

Evidence for the `ARGON-UPS-HID-*` facts in [`../FACTS.md`](../FACTS.md).

## What was captured

The HID report descriptor of the Argon PWR UPS, read from sysfs. No device write of any
kind; the descriptor is published by the kernel from the USB configuration.

```
$ cat /sys/class/hidraw/hidraw0/device/report_descriptor > ups-hid-report-descriptor.bin
```

Artifact: [`ups-hid-report-descriptor.bin`](ups-hid-report-descriptor.bin) (416 bytes; the
sysfs read returns a 4096-byte buffer, trailing padding stripped).

## Provenance

| | |
|---|---|
| Host | Raspberry Pi 5 Model B Rev 1.1, revision `e04171` |
| OS | Debian GNU/Linux 13 (trixie), aarch64, kernel 6.18.39+rpt-rpi-2712 |
| Device | `HID_NAME=Argon Argon_USB`, `HID_PHYS=usb-1000480000.usb-1.1/input2` |
| USB | `1d6b:0104`, iManufacturer `Argon`, iProduct `Argon USB`, iSerial `XXXXXXXXXXXXXXXX` |
| Topology | internal `dwc2` host controller `1000480000.usb`, hub port 1 |
| Date | 2026-09-15 |

The device reported firmware version 113 and `Charging 95%` at capture time.

## What it establishes

The descriptor opens `05 84 09 04 a1 01` — Usage Page `0x84` (Power Device), Usage `0x04`
(UPS), Application Collection — and later switches to Usage Page `0x85` (Battery System).
That is the standard USB HID Power Device class, so the reports below carry their
class-defined meanings per [DS-HIDPD], not Argon-specific ones.

Reports present: `0x01`/`0x02`/`0x03`/`0x1f`/`0x20` (string indices), `0x06` Rechargeable,
`0x07` PresentStatus, `0x08` AtRate, `0x09` ManufacturerDate, `0x0b` ConfigVoltage,
`0x0c` RelativeStateOfCharge, `0x0d` FullChargeCapacity, `0x0e` RemainingCapacity,
`0x0f`, `0x10`/`0x18` CapacityGranularity, `0x11` RemainingCapacityLimit,
`0x12`, `0x13`, `0x14`, `0x16` CapacityMode, `0x17` DesignCapacity, `0x1a` AverageTime,
`0x1c` RunTimeToEmpty.

`0x11` RemainingCapacityLimit is a Feature item with a variable (non-constant) declaration,
i.e. host-writable — the low-battery threshold can be set *in the device*.

## How to re-verify

```
cat /sys/class/hidraw/hidraw0/device/report_descriptor | cmp - ups-hid-report-descriptor.bin
```

A mismatch means the UPS firmware changed; re-run the decode before trusting the report IDs.
