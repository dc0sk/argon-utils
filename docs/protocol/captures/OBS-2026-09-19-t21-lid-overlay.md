---
project: argon-utils
doc: protocol/captures/OBS-2026-09-19-t21-lid-overlay
status: evidence
last_updated: 2026-09-19
---

# OBS-2026-09-19-t21-lid-overlay

Task T21, on the Argon ONE UP. **`argon-oneup-lid.dtbo` gives logind a working lid switch.**

Full output: [`t21-2026-09-19-lid-overlay.log`](t21-2026-09-19-lid-overlay.log).

## Method

`~/t21-lid-overlay.sh`, run by the operator: `argononeupd` stopped; the overlay (0.1.13 build)
loaded at runtime with `dtoverlay -d ~ argon-oneup-lid` -- `config.txt` untouched; `LidClosed`
polled from logind every 0.2 s for 60 s under a `handle-lid-switch` inhibitor, so logind saw the
lid but did not act; then the overlay removed and `argononeupd` started from an exit trap.

## Result

| Time | Observed |
|---|---|
| 09:16:31 | logind: `Watching system buttons on /dev/input/event14 (argon_lid)` |
| 09:16:33 | `LidClosed b false`, lid open |
| 09:16:48 | `LidClosed b true`; logind: `Lid closed.` |
| 09:16:54 | `LidClosed b false`; logind: `Lid opened.` |
| 09:17:30-31 | `LidClosed b true`; logind: `Lid closed.` |

The second opening came after the 60 s window ended, so it is not in the log.

- **The overlay applies at runtime on a CM5** and creates a gpio-keys switch device that logind
  adopts as a lid switch without any configuration.
- **Polarity is right**: closed reads `true`, open `false`.
- **The inhibitor held**: logind logged each change and did nothing about it.
- **Clean up worked**: afterwards no overlay was loaded and `argononeupd` held GPIO27 again.
- **Naming**: the input device was named after the node, `argon_lid`, not the key's label --
  gpio-keys takes the device name from the parent's `label`. Fixed in the overlay afterwards
  (0.1.14); the check that grepped for the label printed "NO INPUT DEVICE" for that reason
  only.

## Not tested

Booting with the overlay in `config.txt`, and logind acting on the lid (`HandleLidSwitch=`).
