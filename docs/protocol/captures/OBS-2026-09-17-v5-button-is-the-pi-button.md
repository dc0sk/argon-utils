---
project: argon-utils
doc: protocol/captures/OBS-2026-09-17-v5-button-is-the-pi-button
status: evidence
last_updated: 2026-09-17
---

# OBS-2026-09-17-v5-button-is-the-pi-button

Task T2. **On an Argon ONE V5 with a Pi 5, the case power button is the Raspberry Pi 5's own
dedicated power button.** No Argon electronics sit in the path. Presses reach Linux as
`KEY_POWER` from the `pwr_button` gpio-keys device, and the desktop handles them.

This matches the fan finding the day before: on this hardware there is no Argon MCU at all.

## Evidence

- The user confirmed the case button is the Pi's dedicated power button.
- `pwr_button` exists as `/dev/input/event6`, bound to `PWR_GPIO` (RP1 line 20).
- Pressing it shut the machine down through the desktop's handler, which only acts on
  `KEY_POWER`, so the press demonstrably reached Linux that way.

## How the Pi desktop handles the key

labwc binds it in `/etc/xdg/labwc/rc.xml`:

```xml
<keybind key="XF86PowerOff" onRelease="yes">
  <action name="Execute"><command>pwrkey</command></action>
</keybind>
```

`/usr/bin/pwrkey`:

```sh
if ! pgrep -f pishutdown ; then
  /usr/bin/pishutdown           # first press: open the shutdown dialog
else
  if ! raspi-config nonint is_pi500 ; then
    /usr/bin/pkill orca
    /sbin/shutdown -h now       # press again while the dialog is open: shut down now
  fi
fi
```

The desktop also holds a logind inhibitor, `rpi-gui-nop` (`handle-power-key`, `block`), so
that logind stays out of the way and only this handler acts.

## What happened during the test

The test was set up on the assumption that a logind `handle-power-key` inhibitor would stop
a shutdown. **That assumption was wrong, and the machine shut down**, including the session
running the test. Nothing was lost: the repository was clean and passed `git fsck` after
the reboot.

The inhibitor did nothing because logind was never going to act; the desktop's own
inhibitor already guaranteed that. The shutdown came from labwc → `pwrkey`, which is
outside logind entirely.

The sequence was one click, then a hold of about a second. Going by the script, the click
opened the dialog, and releasing the hold counted as a second press while the dialog was
open, which ran `shutdown -h now`. That order can't be confirmed from logs: the watcher
output was in `/tmp`, which is tmpfs, and the previous boot's journal was not retained.

**For any future test of this button:** a logind inhibitor does not protect you on the Pi
desktop. Save your work first, or test from a text console where labwc is not running.

## Consequences

- **GPIO 4 being unheld is not a vendor bug on this hardware.** There is no MCU to send
  pulses on it. `argonctl doctor` had been warning that "no process is listening for button
  presses", which was wrong. It now identifies the Pi's own button and says so.
- **Task T3 (measuring MCU pulse widths) moves to the Pi 4 / ONE V2**, the same as T5.
  The ONE V5 has no pulses to measure.
- **There is nothing here for argon-utils to take over.** The OS already handles the button
  sensibly with a confirmation dialog. As with the fan, the useful thing to do on this
  machine is to leave it alone.
- On the ONE V5 with a Pi 5, the Argon hardware argon-utils can usefully drive comes down to
  the **UPS** (serial), the **OLED** (I2C `0x3c`) and the **Zigbee** module (USB).
