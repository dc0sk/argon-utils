---
project: argon-utils
doc: protocol/FACTS
status: living
last_updated: 2026-09-15
---

# Protocol fact ledger

Every hardware fact `argon-utils` relies on has an entry here with a stable ID and a
provenance status. Implementation code cites these IDs. See [`CLEANROOM.md`](../../CLEANROOM.md)
for why this exists.

## Provenance statuses

| Status | Meaning | May back an implementation? | May back a **write**? |
|---|---|---|---|
| `documented` | Stated in Argon40's public protocol documentation or a component datasheet | yes | yes |
| `observed` | Measured by us on hardware we own, with the capture committed as evidence | yes | yes |
| `inferred` | Derived from reading upstream implementation source | **no** | **no** |
| `unknown` | Named somewhere, semantics undetermined | **no** | **no** |

`inferred` facts are listed so we know what to go and verify. They must be promoted to
`observed` — by observing *the device*, never by re-reading the code — before use. An
`unknown` fact must not be exposed in any public API.

## Sources

- **[DOC-I2C]** [`Argon40Tech/Argon-ONE-i2c-Codes`](https://github.com/Argon40Tech/Argon-ONE-i2c-Codes) — Argon40's published MCU command list.
- **[DS-SSD1306]** Solomon Systech SSD1306 datasheet rev 1.1.
- **[DS-PCF8563]** NXP PCF8563 datasheet.
- **[DS-HIDPD]** USB-IF *Usage Tables for HID Power Devices* rev 1.0.
- **[DOC-TI-MT]** Texas Instruments, *Z-Stack Monitor and Test API* -- the published serial
  protocol of TI's Z-Stack coordinator firmware (framing, `SYS_*` commands). Describes the
  firmware, not the Argon module: each fact still needs confirming on this module (task T16).
- **[DS-CW2217]** Cellwise, *CW2217BAAD datasheet* V0.1 (2021), as published through a
  distributor: <https://uploadcdn.oneyac.com/attachments/files/brand_pdf/cellwise/06/EB/2022040752158413.pdf>.
  Describes the chip, not the ONE UP board: the sense resistor, battery profile and whether
  this chip is on the board at all are not in it.
- **[FORUM-ONEUP-CW2217]** A user post (not Argon staff), 2026-02-13, citing "the block diagram
  provided with the hardware": <https://forum.argon40.com/t/argon-one-up-native-battery-integration/9146>.
  A claim of which part is fitted -- no code was read from it.
- **[OBS-<date>-<topic>]** our own captures, under `docs/protocol/captures/`.

---

## ARGON-MCU-* — ONE-family MCU, I2C bus 1, address 0x1a

| ID | Fact | Status | Source |
|---|---|---|---|
| `ARGON-MCU-ADDR` | MCU responds at I2C address `0x1a` | `documented` | [DOC-I2C] |
| `ARGON-MCU-ABSENT-V5` | **On an Argon ONE V5 with a Pi 5 there is no device at `0x1a` at all.** The fan is on the Pi's own 4-pin header, driven by the kernel `pwm-fan` cooling device | `observed` | OBS-2026-09-16-v5-fan-is-kernel-controlled |
| `PI5-FAN-TAKEOVER-COST` | Taking manual control of the Pi 5 fan requires disabling `thermal_zone0` entirely, which also disables the 110 °C critical trip. There is no runtime way to detach one cooling device from a zone | `observed` | OBS-2026-09-16-taking-the-pi5-fan |
| `ARGON-OLED-PRESENT-V5` | The OLED does respond at `0x3c` on that machine | `observed` | same |
| `ARGON-MCU-L-FAN` | Raw `write_byte` of `0x00` stops the fan; `0x01`–`0x64` sets duty cycle as a literal percent | `documented` | [DOC-I2C] |
| `ARGON-MCU-L-FANMIN` | The fan does not physically start turning below ~10% duty | `documented` | [DOC-I2C] |
| `ARGON-MCU-L-MODE1` | `0xFD` selects "default mode": a button press is required to power on after shutdown or power loss | `documented` | [DOC-I2C] |
| `ARGON-MCU-L-MODE2` | `0xFE` selects "always on" mode: power flows to the Pi without a button press | `documented` | [DOC-I2C] |
| `ARGON-MCU-L-PWRCUT` | `0xFF` arms power-cut; the MCU then monitors UART TX (BCM 14) voltage and cuts power when it goes low. Requires the serial port enabled | `documented` | [DOC-I2C] |
| `ARGON-MCU-L-IR` | `0xAA` is the IR-code write command | `documented` | [DOC-I2C] |
| `ARGON-MCU-L-BOOTLOADER` | `0xBB` enters the firmware bootloader | `documented` | [DOC-I2C] |
| `ARGON-MCU-R-DUTY` | Register `0x80` reads/writes fan duty as a percent | `inferred` | — |
| `ARGON-MCU-R-IR` | Register `0x82` accepts an IR code as a block write | `inferred` | — |
| `ARGON-MCU-R-CTRL` | Register `0x86`, written with `1`, signals power off | `inferred` | — |
| `ARGON-MCU-R-FW` | Register `0x81` — semantics undetermined | `unknown` | — |
| `ARGON-MCU-R-RESERVED` | Registers `0x83`–`0x85` — undetermined. Do not touch | `unknown` | — |
| `ARGON-MCU-FAILSAFE` | Whether the MCU reverts to a safe duty if the host stops writing | `unknown` | — |
| `ARGON-MCU-PERSIST` | Whether duty or power mode persist across power loss | `unknown` | — |

> **`ARGON-MCU-HAZARD` (`documented`, derived from `ARGON-MCU-L-FAN`).** SMBus
> `read_byte_data(0x1a, reg)` places `reg` on the bus as a write before the repeated start.
> On firmware implementing only `ARGON-MCU-L-FAN`, reading register `0x80` is therefore
> indistinguishable from writing fan duty `0x80` = 128, which clamps to 100%.
> **A register read is as destructive as a register write.** There is no safe software probe
> for which dialect the MCU speaks. See ADR-002.

## ARGON-GPIO-*

| ID | Fact | Status | Source |
|---|---|---|---|
| `ARGON-GPIO-BTN` | The case power button is wired to the MCU on BCM 17; the MCU signals the host on BCM 4. **Pi 4-era cases only** | `documented` | [DOC-I2C] |
| `ARGON-UPS-CMD3-FW17` | **Firmware 17 accepts the clock write** (`CMD3`) as firmware 113 does: T15 in full mode moved an Industria UPS 5000's clock from 18.4 years slow to within +1 s. T15's printed read-back of its deliberately wrong time was not seen, so the negative control is unconfirmed; the end state is | `observed` | OBS-2026-09-23-neo5-industria-ups-5000 |
| `ARGON-UPS-NO-DFU` | Neither UPS exposes a USB DFU interface (`fe/01`) in normal operation, and no serial command updates firmware. How the firmware *is* updated is **unknown**; see [`ups-firmware.md`](ups-firmware.md). Not probed for, and no update mechanism will be built | `observed` (absence), `unknown` (the mechanism) | OBS-2026-09-23-neo5-industria-ups-5000 |
| `ARGON-UPS-FW-17` | An **Industria UPS 5000** answers the same serial protocol as the PWR UPS 10000 -- battery, firmware, clock, wake schedule -- and reports **firmware 17** where the tested 10000 unit reports 113. Reads only; nothing about writes on this firmware is established | `observed` | OBS-2026-09-23-neo5-industria-ups-5000 |
| `ARGON-UPS-INTERNAL-HEADER-DWC2` | The case's internal USB header needs `dtoverlay=dwc2,dr_mode=host` **in force for the running board**. On a Pi 5 a `[cm5]`-filtered line does not apply, the controller stays down, and the UPS never enumerates | `observed` | OBS-2026-09-23-neo5-industria-ups-5000 |
| `ONE-V1-IR-BCM23` | On an **Argon ONE V1** with a Pi 4, an IR receiver is wired to BCM 23 and works with the stock `gpio-ir` overlay -- no vendor software involved | `observed` | OBS-2026-09-22-t6-one-v1-ir |
| `ONE-V1-IR-NEC` | Its remote speaks **NEC, address `0x00`**: 9 ms/4.5 ms header, 32 bits LSB first, 9 ms/2.25 ms repeats. Ten commands read with valid inverse checks: `0x80`, `0x81`, `0x90`, `0x99`, `0x9c`, `0x9d`, `0xc1`, `0xca`, `0xcb`, `0xd2`. Which button sends which is **not** established | `observed` | OBS-2026-09-22-t6-one-v1-ir |
| `ONE-V1-IR-KEYMAP` | The Argon remote's eleven buttons at address `0x00`: power `0x9c`, up `0xca`, down `0xd2`, left `0x99`, right `0xc1`, OK `0xce`, menu `0x9d`, back `0x90`, home `0xcb`, vol+ `0x80`, vol- `0x81`. Measured by pressing each three times in a stated order; a second, unlabelled capture agrees on the set | `observed` | OBS-2026-09-22-t6b-one-v1-ir-keymap |
| `ONE-V1-IR-POWER-INERT` | Pressing power on the remote does **not** power the machine off: the case acts on a held case button in hardware, but not on IR | `observed` | OBS-2026-09-22-t6b-one-v1-ir-keymap |
| `ONE-V1-MCU-LEGACY` | The device at `0x1a` on an **Argon ONE V1** speaks the **legacy single-byte protocol**: a register *read* of `0x80` set the fan to full and it stayed there, which is `0x80` read as a duty of 128. Legacy duty bytes moved the fan in both directions | `observed` | OBS-2026-09-22-t5-mcu-dialect |
| `ONE-V1-MCU-READ-GARBAGE` | That register read returned `0xc9` (201) -- not a duty (0-100), not the 25 just written. This firmware does not answer register reads meaningfully | `observed` | OBS-2026-09-22-t5-mcu-dialect |
| `ONE-V1-MCU-PRESENT` | On an **Argon ONE V1** with a Pi 4, a device at I2C `0x1a` acknowledges an SMBus quick-write (address + write bit, no data byte). Its firmware, dialect and function are **not** established by this: an address ACK is one bit | `observed` | 2026-09-22, `argonctl fan --probe`; negative control on the ONE V5, which does not answer |
| `ONE-V1-BTN-PULSE` | On an **Argon ONE V1** with a Pi 4, the case signals a button press as one pulse on BCM 4 of **20085-20104 us** (23 events, one outlier at 20236). The width is fixed: a single tap and a double-tap are indistinguishable, and presses under ~1 s apart merge into one pulse | `observed` | OBS-2026-09-22-t3-button-pulses |
| `ONE-V1-BTN-HOLD-SILENT` | A press held ~2 s produces **no edge at all** on BCM 4: holding is the case cutting power in hardware, not a signal to the host | `observed` | OBS-2026-09-22-t3-button-pulses |
| `ARGON-BTN-V5-IS-PI` | **On an Argon ONE V5 with a Pi 5 the case button is the Pi's own power button.** It arrives as `KEY_POWER` from `pwr_button`; no MCU is involved | `observed` | OBS-2026-09-17-v5-button-is-the-pi-button |
| `ARGON-GPIO-IRRX` | IR receiver on BCM 23 | `documented` | [DOC-I2C] |
| `ARGON-GPIO-IRTX` | IR transmitter on BCM 22 | `documented` | [DOC-I2C] |
| `ARGON-GPIO-UARTMON` | The MCU monitors BCM 14 (UART TX) for the `0xFF` power-cut mechanism | `documented` | [DOC-I2C] |
| `ARGON-GPIO-BTN-WINDOWS` | Pulse widths on BCM 4 encode reboot / shutdown / display-switch | `inferred` | — |
| `ARGON-GPIO-LID` | ONE UP lid switch on BCM 27, pull-up, 0 = closed | `inferred` | — |

`ARGON-GPIO-BTN-WINDOWS` is deliberately being re-established by our own measurement rather
than promoted from upstream's numbers: theirs were derived from a `sleep(0.01)` accumulation
loop that drifts under load, so our measurement is expected to be the better specification.

## ARGON-UPS-* — Argon PWR UPS

### Serial transport (CDC-ACM)

| ID | Fact | Status | Source |
|---|---|---|---|
| `ARGON-UPS-SERIAL-PARAMS` | 115200 8N1 on the CDC-ACM interface | `observed` | OBS-2026-09-15-ups-ident |
| `ARGON-UPS-FRAME` | Frame is `0xFE \| len \| cmd \| payload… \| checksum`, checksum = sum of all preceding frame bytes `& 0xFF` | `observed` | OBS-2026-09-17-ups-serial-capture |
| `ARGON-UPS-FRAME-AMBIGUITY` | **The framing has no escape mechanism.** `0xFE` is legal inside a length field, so a single stray start byte immediately before a frame is read as a length of 254 and swallows up to 259 following bytes | `observed` | property testing, 2026-09-15 |
| `ARGON-UPS-READSHORT` | A pure read is the 4-byte frame `FE 00 <cmd> <(cmd+0xFE)&0xFF>` | `observed` | same |
| `ARGON-UPS-CMD0` | Command 0 returns `[percent, charging]`; `charging == 0` means on mains | `observed` | same — read 91% / on-mains, matching the vendor daemon |
| `ARGON-UPS-CMD2` | Command 2 returns a 16-bit big-endian value; read 850 while charging at 91%. **Units still undetermined** | `observed` (framing) / `unknown` (units) | same |
| `ARGON-UPS-CMD3` | Command 3 sets the RTC from 6 BCD bytes `YY MM DD HH MM SS`, UTC | `observed` | T15, 2026-09-18: a distinctive wrong time (`FE 06 03 26 09 18 10 01 33 92`) read back within 1 s, then the correct time restored the same way -- `OBS-2026-09-18-t15-rtc-set` |
| `ARGON-UPS-CMD3-REPLY` | **The device answers a set with an empty command-3 frame**, `FE 00 03 01` -- not a command-8 acknowledgement | `observed` | same, both sets |
| `ARGON-UPS-CMD4` | Command 4 returns a 1-byte firmware version | `observed` | same — read 113 |
| `ARGON-UPS-CMD5` | Command 5 returns the RTC as 6 BCD bytes, UTC | `observed` | same — read 2026-09-17 13:29:15 UTC |
| `ARGON-UPS-CMD6` | Command 6 sets an absolute wake schedule from 5 BCD bytes `YY MM DD HH MM`, UTC | `observed` | T17, 2026-09-18: two far-future times, different in every field, each read back exactly -- `OBS-2026-09-18-t17-wake-set` |
| `ARGON-UPS-CMD6-REPLY` | The device answers a wake set with an empty command-6 frame, `FE 00 06 04` | `observed` | same, both sets |
| `ARGON-UPS-SET-ECHO` | **Pattern:** both set commands seen so far (3 and 6) are answered by an empty frame echoing the command. Not assumed for any other command | `observed` for 3 and 6 only | T15, T17 |
| `ARGON-UPS-WAKE-MECHANISM` | A schedule set with CMD6 powers the machine on at that minute, from a halted Pi (`POWER_OFF_ON_HALT`, `WAKE_ON_GPIO=0`): asked for 19:14, up at 19:14:22 | `observed` | T18, 2026-09-20 |
| `ARGON-UPS-WAKE-CLEAR` | The UPS clears a schedule once it has fired: argond found none five seconds after the wake boot, so a wake never repeats | `observed` | T18, 2026-09-20 |
| `ARGON-UPS-CMD7` | Command 7 returns the wake schedule as 5 BCD bytes | `observed` | same |
| `ARGON-UPS-CMD7-EMPTY` | **With no schedule set, command 7 answers with an EMPTY payload** (`FE 00 07 05`), not five zero bytes | `observed` | same |
| `ARGON-UPS-CMD8` | Command 8 is device-initiated; the host echoes it back as an acknowledgement | `inferred` | — |
| `ARGON-UPS-CMD9` | Command 9 resets the battery meter. **Destructive** — discards the meter baseline | `inferred` | — |
| `ARGON-UPS-CMD-UNMAPPED` | Command IDs above 9 are unmapped. **Never sweep the command space** — 9 is already destructive | `unknown` | — |

### HID transport (USB HID Power Device)

The UPS exposes a HID interface alongside CDC-ACM. The two are independent: reading HID does
not contend with the serial port.

| ID | Fact | Status | Source |
|---|---|---|---|
| `ARGON-UPS-HID-CLASS` | The HID interface is a standard USB HID Power Device: Usage Page `0x84` (Power Device) + `0x85` (Battery System), `Usage(UPS)`. 416 bytes, 198 items, balanced collections | `observed` | OBS-2026-09-15-ups-hid-descriptor |
| `ARGON-UPS-HID-TABLE` | The full report/usage/size/range/flags table, extracted mechanically from the descriptor | `observed` | same, via `tools/hid-report-table.py` |
| `ARGON-UPS-HID-SEMANTICS` | The mapping from usage number to meaning (e.g. `0x85:0x66` → RelativeStateOfCharge) | `documented` **pending** | [DS-HIDPD] — **must be re-derived from the USB-IF PDF before code depends on it** |
| `ARGON-UPS-HID-WRITABLE` | Which Feature items are host-writable (declared `Data`, not `Const`) — see the table below | `observed` | same |
| `ARGON-UPS-HID-VOLATILE` | Nearly every item, **including the low-battery threshold `0x11`, is declared VOLATILE** | `observed` | same |
| `ARGON-UPS-HID-NUT` | Stock NUT 2.8.1 **cannot** drive this UPS: `usbhid-ups` looks for HID on interface 0, the UPS has it on interface 2, and the driver has no interface-selection option | `observed` | OBS-2026-09-15-nut-usbhid-ups |
| `ARGON-UPS-HID-LIVE` | **The HID interface serves no data on firmware 113, unconditionally.** Confirmed across a real mains-removal transition: serial saw the change immediately, HID emitted nothing in 60 s and feature reads still returned only the echoed report ID | `observed` | OBS-2026-09-15-ups-hid-is-dormant |
| `ARGON-UPS-HID-PARSER-XCHECK` | The Linux kernel's HID parser and `argon-proto`'s derive identical report IDs, field counts and logical ranges from the same descriptor | `observed` | same |
| `ARGON-UPS-HID-ACCESS` | HID telemetry must be read via **`hidraw`**, never libusb. A libusb interface claim detaches the kernel driver and breaks `/dev/ttyACM0` until the device is re-enumerated; hidraw does not | `observed` | same |
| `ARGON-UPS-USBID` | The UPS enumerates as `1d6b:0104` — the *generic Linux USB gadget* VID:PID. It must be identified by string descriptors (`Argon` / `Argon USB` / serial), never by VID:PID | `observed` | OBS-2026-09-15-ups-ident |

## ZIGBEE-* — Argon Industria Zigbee module (CC2652P behind a CP2102N)

| ID | Fact | Status | Source |
|---|---|---|---|
| `ZIGBEE-USB` | A CP2102N (`10c4:ea60`) with Silicon Labs' generic strings and serial `xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`, on port 2 of the case's internal hub (`1-1.2`, controller `1000480000.usb`) | `observed` | `udevadm info`, 2026-09-18 |
| `ZIGBEE-CP2102N-GPIO` | The bridge exposes 7 GPIO lines (`gpiochip14`), all idle inputs | `observed` | `gpioinfo`, 2026-09-18 |
| `ZIGBEE-LINES` | Opening the port, with DTR then RTS released straight away, does **not** restart the radio: no `SYS_RESET_IND` within 3 s. Whether the lines are unwired, or wired but the pulse too short, is not established | `observed` (the practical fact); wiring `unknown` | OBS-2026-09-18-t16-zigbee-probe |
| `ZIGBEE-BAUD` | Z-Stack's serial interface runs at 115200 8N1, no flow control | `documented`, confirmed working | [DOC-TI-MT], OBS-2026-09-18-t16-zigbee-probe |
| `ZIGBEE-MT-FRAME` | Frames are `FE LEN CMD0 CMD1 DATA FCS`, FCS the XOR of LEN through the last data byte | `observed` | both replies in T16 carried valid FCS |
| `ZIGBEE-SYS-PING` | `SYS_PING` is `21 01`; the reply `61 01` carries 2 capability bytes, little-endian. This module: `FE 02 61 01 59 06 3D`, capabilities `0x0659` (bits not decoded here) | `observed` | T16 |
| `ZIGBEE-SYS-VERSION` | `SYS_VERSION` is `21 02`; the reply `61 02` carries transport rev, product, major, minor, maint and a 4-byte little-endian revision. This module: transport 2, product 1, release 2.7.1, revision 20230507 | `observed` | T16 |
| `ZIGBEE-SYS-VERSION-TAIL` | The reply carries **10** data bytes, one more than the documented layout: a trailing `0x00` of unknown meaning | `observed`; meaning `unknown` | T16 |
| `ZIGBEE-RESET-IND` | The firmware sends `41 80` (`SYS_RESET_IND`) unprompted after every restart | `documented` | [DOC-TI-MT] |
| `ZIGBEE-FIRMWARE` | The radio runs firmware that speaks Z-Stack MT: it answers `SYS_PING` and `SYS_VERSION`. Which build it is beyond the version fields is not established here | `observed` | T16 |

The module has no datasheet in Argon's published set (checked 2026-09-18), so nothing about
its board-level wiring is `documented`.

## ONEUP-* — Argon ONE UP (CM5 laptop)

One unit, surveyed passively on 2026-09-18 with the vendor's daemon running
([OBS-2026-09-18-one-up-survey](captures/OBS-2026-09-18-one-up-survey.md)). Nothing here
backs a write.

| ID | Fact | Status | Source |
|---|---|---|---|
| `ONEUP-I2C-PRESENCE` | I2C bus 1 has exactly one device, at `0x64`. Nothing answers at `0x1a` | `observed` | presence scan, i2cdetect default mode |
| `ONEUP-0x64-IDENTITY` | The `0x64` device is a Cellwise **CW2217** battery fuel gauge: the CW2217BAAD's fixed address is `0x64`, and its VERSION register (`0x00`) read `0xA0`, the value the datasheet fixes | `observed` | [FORUM-ONEUP-CW2217] (lead), [DS-CW2217], OBS-2026-09-18-one-up-survey |
| `ONEUP-GAUGE-READS` | SOC, VCELL, CURRENT, CONFIG, cycles and SOH read back plausibly: 100 %, 4.397 V, ~0, active, 24, 100 % | `observed` | OBS-2026-09-18-one-up-survey |
| `ONEUP-GAUGE-TEMP` | TEMP reads `0x82` = 25.0 °C, the reset default, unchanging. Whether a thermistor drives it is `unknown`; not to be reported as a measurement until seen to move | `observed` (value); meaning `unknown` | OBS-2026-09-18-one-up-survey |
| `ONEUP-PACK` | VCELL is a single cell's voltage (4.40 V) while the pack is rated 55 Wh / 4800 mAh (~11.5 V, three cells). How the gauge sees the pack is `unknown` | `observed` / `unknown` | OBS-2026-09-18-one-up-survey |
| `ONEUP-CURRENT-SIGN` | On battery, CURRENT reads negative (about -2100 to -3000 LSB with the machine running); on the charger, positive (+2875 falling to +2688 while topping up), as documented | `observed` | OBS-2026-09-18-one-up-survey |
| `ONEUP-GAUGE-BURST` | A two-byte read in one transaction from `0x02`, `0x04` or `0x0E` returns the same pair as two single-byte reads | `observed` | OBS-2026-09-18-one-up-survey |
| `ONEUP-LID-PANEL` | The internal panel (on `HDMI-A-2`) goes dark while the lid is closed, with nothing in the kernel log and the connector still `connected`/`enabled`. On opening, labwc logged `Swapchain for output 'HDMI-A-2' failed test` and left the screen black until the output was power-cycled (a `wlopm` off/on, or a VT switch). Mechanism `unknown` | `observed` | T22, 2026-09-19 |
| `ONEUP-KEYBOARD-USB` | The built-in keyboard is two USB devices on the internal hub: `6080:8061` (port 1-1.6: keyboard, mouse, touchpad) and `6080:8060` (port 1-1.7: keyboard, mouse, system control, wireless-radio control, consumer control). On one boot (after a lid-triggered poweroff) 1-1.7 failed four descriptor reads (`-110`) and enumerated about a minute late | `observed` | T22, 2026-09-19 |
| `ONEUP-KEYBOARD-LIGHT` | Fn+Space reaches the host as `KEY_F16` (scancode `0x7002B`) on the 1-1.7 keyboard, identically for on and off; no LED device appears, so the keyboard drives its own backlight and the host cannot set or read it | `observed` | T24, 2026-09-20 |
| `ONEUP-RSENSE` | The current-sense resistor, which scales CURRENT to amperes | `unknown` | -- |

What the CW2217 datasheet documents [DS-CW2217] -- true of the chip, and so of the ONE UP's
gauge, though each is `documented` rather than `observed` until read there:

| ID | Fact | Status |
|---|---|---|
| `CW2217-READ` | A register read is `S addr+W reg Sr addr+R data P`. The written byte only sets the register pointer; a write needs a data byte after it | `documented` |
| `CW2217-VERSION` | `0x00`, read-only, fixed `0xA0` in both active and shutdown mode | `documented` |
| `CW2217-VCELL` | `0x02`-`0x03`, 14-bit unsigned, 312.5 µV per LSB | `documented` |
| `CW2217-SOC` | `0x04` = whole percent; `0x05` = 1/256 % | `documented` |
| `CW2217-TEMP` | `0x06`, °C = value / 2 - 40 (NTC at the TS pin) | `documented` |
| `CW2217-CURRENT` | `0x0E`-`0x0F`, signed 16-bit two's complement; positive = charging (`0x7FFF` max charge, `0x8001` max discharge); A = 52.4 × value / (32768 × Rsense mΩ) | `documented` |
| `CW2217-CYCLES-SOH` | `0xA4`-`0xA5` charge cycles, `0xA6` state of health, both read-only | `documented` |
| `CW2217-CONFIG` | `0x08` holds SLEEP and RESTART. Writing it puts the gauge to sleep or restarts its SOC calculation. **Never written by this project** | `documented` |
| `CW2217-PROFILE` | The battery profile and `SOC_ALERT` bit 7 (`UPDATE_FLAG`) are the integrator's configuration. **Never written by this project** | `documented` |
| `ONEUP-NO-KERNEL-BATTERY` | No `power_supply` class device; no kernel driver bound on any I2C bus | `observed` | sysfs |
| `ONEUP-FAN` | The fan is the kernel's `pwm-fan` (line `FAN_PWM`), as on a Pi 5 in the ONE V5 | `observed` | sysfs, `gpioinfo` |
| `ONEUP-POWER-KEY` | The power key is the Pi's own (`pwr_button` on `PWR_GPIO`) | `observed` | `/sys/class/input`, `gpioinfo` |
| `ONEUP-GPIO27` | The **lid switch**: with the pull-up, high while open, low while closed, one clean edge per movement (no bounce). Held by the vendor daemon with both-edge events | `observed` | `gpioinfo`; OBS-2026-09-19-t20-one-up-lid |
| `ONEUP-NO-USB-UPS` | No UPS on USB: the PWR UPS serial protocol does not apply | `observed` | `lsusb`, hidraw names |

## ARGON-OLED-* / ARGON-RTC-*

| ID | Fact | Status | Source |
|---|---|---|---|
| `ARGON-OLED-ADDR` | SSD1306 panel at I2C `0x3c`, 128×64, normal orientation | `observed` | OBS-2026-09-17-oled-bring-up |
| `ARGON-OLED-INIT` | Power-on initialisation sequence | `documented` [DS-SSD1306], confirmed working on hardware | OBS-2026-09-17-oled-bring-up |
| `ARGON-RTC-EON-ADDR` | EON carries a PCF8563 at I2C `0x51` | `inferred` | — |
| `ARGON-RTC-EON-REGS` | PCF8563 register layout and BCD encoding | `documented` | [DS-PCF8563] |

No EON hardware is available to this project, so every EON fact stays `inferred` and its
capability ships marked `untested-hardware`.

### ARGON-UPS-HID-TABLE — the writable Feature items

Extracted mechanically by [`tools/hid-report-table.py`](tools/hid-report-table.py) from the
committed descriptor. Only items declared `Data` (as opposed to `Const`) are host-settable;
`Const` items are report-only regardless of being Feature items.

| Report | Page | Usage | Size | Range | Volatile? | Likely meaning (**unconfirmed**) |
|---|---|---|---|---|---|---|
| `0x08` | 0x85 | `0x2a` | 16 | 120–1380 | yes | RemainingTimeLimit — a second, time-based low-battery threshold |
| `0x0c` | 0x85 | `0x66` | 8 | 0–100 | no | RelativeStateOfCharge — also an **Input** report |
| `0x0f` | 0x85 | `0x8c` | 8 | 0–100 | yes | — |
| `0x10` | 0x85 | `0x8d` | 8 | 0–100 | no | CapacityGranularity1 |
| `0x11` | 0x85 | `0x29` | 8 | 0–100 | **yes** | **RemainingCapacityLimit — the low-battery threshold** |
| `0x12` | **0x84** | `0x57` | 16 | i16 | yes | **DANGEROUS — plausibly DelayBeforeShutdown** |
| `0x13` | **0x84** | `0x55` | 16 | i16 | yes | **DANGEROUS — plausibly DelayBeforeStartup** |
| `0x14` | 0x84 | `0x5a` | 8 | 1–3 | yes | AudibleAlarmControl (beeper) |
| `0x16` | 0x85 | `0x2c` | 8 | 0–1 | no | CapacityMode |
| `0x07` | 0x85 | `0x43` | 1 | 0–1 | yes | BatteryPresent bit |
| `0x07` | **0x84** | `0x68` | 1 | 0–1 | yes | — |

> **`ARGON-UPS-HID-DANGER` — safety rule.** Reports `0x12` and `0x13` are writable 16-bit
> items on the **Power Device** page. If the usage numbers mean what they appear to, writing
> them instructs the UPS to cut or restore its own output after a delay — i.e. they can power
> the host off. Their semantics are `unknown` until confirmed against the USB-IF
> specification, and per the provenance rules an `unknown` fact may not back a write.
> **`argon-utils` does not write reports `0x12` or `0x13` in any version**, and does not
> expose them in any API. They are documented here so that a future contributor recognises
> them as hazardous rather than as unclaimed features.

> **`ARGON-UPS-HID-PERSIST`.** Report `0x11` is declared **Volatile**. That means a
> device-side low-battery threshold survives our daemon crashing, being killed, or never
> starting — which is still a stronger guarantee than a host-side value — but the descriptor
> does **not** establish that it survives a UPS power cycle, and the Volatile flag argues
> against it. Status: `unknown`. The daemon must therefore re-assert the configured value on
> every startup and on every UPS reconnect, and log any drift it finds. That logging is
> itself the field experiment that answers the question.


## ARGON-UPS-FRAME-AMBIGUITY — a limitation worth understanding

Found by property testing, from the minimal counterexample `junk = [0xFE]`.

The frame format is `0xFE | len | cmd | payload | checksum` with **no escaping and no
length-field validation**. `0xFE` is a perfectly legal length. So a spurious start byte
arriving immediately before a real frame is consumed as the frame's start, and the real
frame's own `0xFE` becomes a declared length of 254 — which swallows the frame behind it and
up to 259 subsequent bytes while the decoder waits for a payload that never comes.

This is a property of the protocol, not of any particular decoder. No framing-layer fix is
available: there is nothing in the byte stream that distinguishes a start byte from a length
byte that happens to be `0xFE`.

**What bounds it in practice.** `SerialLink::request` resets the decoder before every
exchange and bounds the whole exchange with an operation deadline. A desync therefore costs
one request, which times out and is retried, rather than wedging the link. That is the
guarantee the transport actually relies on, and it is asserted directly by
`a_reset_always_recovers_the_reader`.

**Consequences for anything built on this protocol.** Do not stream-parse it continuously and
assume synchronisation is maintained. Reset per exchange. If a future use needs a continuous
stream — an unsolicited-event listener, say — it needs a resynchronisation strategy of its
own, and the honest options are a time-based gap heuristic or rewinding past a failed frame's
start byte. Neither is implemented, because nothing currently needs it.
