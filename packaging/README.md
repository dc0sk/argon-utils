---
project: argon-utils
doc: packaging/README
status: living
last_updated: 2026-09-15
---

# Packaging

## udev rules

[`udev/60-argon-utils.rules`](udev/60-argon-utils.rules) gives stable device names and
group access:

| Symlink | Device | Why it exists |
|---|---|---|
| `/dev/argon-ups` | UPS CDC-ACM serial | `ttyACM*` numbering is not stable |
| `/dev/argon-ups-hid` | UPS HID interface | `hidraw*` is `root:root 0600` by default |
| `/dev/argon-zigbee` | Industria Zigbee module | `10c4:ea60` is far too common to identify it |

Install:

```sh
sudo groupadd -f argon && sudo usermod -aG argon "$USER"
sudo cp packaging/udev/60-argon-utils.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
# log out and back in for the group change to take effect
```

Verify:

```sh
ls -l /dev/argon-*
argonctl doctor
```

### Checking the rules before installing

```sh
sudo udevadm verify packaging/udev/60-argon-utils.rules
```

This **fails until the `argon` group exists**, because `udevadm verify` resolves the
`GROUP=` names. That failure is correct rather than a syntax problem — create the group
first, or check syntax alone with:

```sh
sudo udevadm verify --resolve-names=never packaging/udev/60-argon-utils.rules
```

### The matching is deliberately not by VID:PID

Both Argon devices are unidentifiable by VID:PID, for different reasons:

- The **UPS** enumerates as `1d6b:0104` — the generic Linux Foundation USB gadget IDs, shared
  with any Linux composite-gadget device. Matched on its string descriptors instead.
- The **Zigbee module** is a CP2102N (`10c4:ea60`), one of the most common USB-serial bridges
  there is. The development machine has two more belonging to an amateur radio transceiver.
  Matched on its unique serial, with internal-hub position as a commented fallback.

If your Zigbee module's serial differs from the one in the rule, read yours from
`argonctl doctor` and edit it — or switch to the topology fallback, which matches by position
on the case's internal hub rather than by identity.

## UPS low-battery shutdown

Three pieces, because the daemon and the desktop are on opposite sides of a boundary:

| File | Install to | Purpose |
|---|---|---|
| [`polkit/50-argon-utils.rules`](polkit/50-argon-utils.rules) | `/etc/polkit-1/rules.d/` | lets the unprivileged `argon` user schedule and cancel a poweroff |
| [`systemd/argond.service`](systemd/argond.service) | `/etc/systemd/system/` | the daemon; owns the UPS serial port |
| [`xdg/argon-notify-agent.desktop`](xdg/argon-notify-agent.desktop) | `/etc/xdg/autostart/` | desktop notifications, started at login |

The daemon only powers the machine off with `mode = "full"`, and only when the vendor's UPS
daemons (`argonupsrtcd`, `argononeupsd`) are not running; otherwise it logs what it would
do. The Raspberry Pi desktop's labwc session runs `lxsession-xdg-autostart`, which is what
starts the agent (checked on the development machine).
