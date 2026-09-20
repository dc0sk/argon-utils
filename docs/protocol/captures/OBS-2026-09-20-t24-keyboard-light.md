---
project: argon-utils
doc: protocol/captures/OBS-2026-09-20-t24-keyboard-light
status: evidence
last_updated: 2026-09-20
---

# OBS-2026-09-20-t24-keyboard-light

Task T24, on the Argon ONE UP. **Fn+Space reaches the system as `KEY_F16`, but the light itself
is the keyboard's own: nothing on the host can switch it.**

Full output: [`t24-2026-09-20-keyboard-light.log`](t24-2026-09-20-keyboard-light.log).

## Method

Every `/dev/input/event*` was watched for 120 s -- read-only, as the login user, who is in the
`input` group; nothing was installed. The operator typed "one two three" as a control and then
pressed Fn+Space twice, switching the keyboard light on and off.

## Result

| Key pressed | Device | `MSC_SCAN` | Key event |
|---|---|---|---|
| `o` (control) | event10 | `0x70012` (HID keyboard page, usage 0x12) | `KEY_O` (24) |
| **Fn+Space**, 1st | event10 | `0x7002B` | **`KEY_F16` (186)**, press and release |
| **Fn+Space**, 2nd | event10 | `0x7002B` | **`KEY_F16` (186)**, press and release |

- **The key is reported** (`ONEUP-KEYBOARD-LIGHT`), from the keyboard on USB port 1.7 -- the
  device that also carries the system, radio and media keys (`ONEUP-KEYBOARD-USB`).
- **Both presses look the same.** The key carries no state, so the host cannot tell whether the
  light went on or off, only that the key was pressed.
- **No LED device appeared** while the light was on: `/sys/class/leds` held the same entries
  before and after (lock-key LEDs only). The keyboard switches its own backlight; the host is
  merely told.
- **So argon-utils cannot switch the light**, and the lid's power-save cannot include it.
  Injecting a synthetic `KEY_F16` would not help: it would reach the host's input stack, not the
  keyboard's internal logic.
- The scancode is odd -- `0x2B` on the HID keyboard page is Tab, which the kernel would normally
  map to `KEY_TAB` -- and it arrives as `KEY_F16` here. Recorded as seen; the mapping is not
  explained.

## What this allows

The host can *react* to the key (a hotkey like any other, e.g. `KEY_F16` in a compositor
binding), but not read or set the light's state.
