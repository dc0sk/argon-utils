---
project: argon-utils
doc: packaging/README
status: living
last_updated: 2026-09-17
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

On an **Argon ONE UP**, set `[ups] source = "oneup"`: argond then reads the laptop's own battery
from its CW2217 fuel gauge on the I2C bus instead, with the same policy, delay and
notifications. There the vendor unit to stop is `argononeupd`, which the package does **not**
retire, because it also handles the lid. `argonctl battery` shows what the gauge reads.

### The ONE UP's lid

The package ships `/usr/share/argon-utils/overlays/argon-oneup-lid.dtbo`, a device-tree overlay
that hands the lid (GPIO27: high open, low closed) to the kernel as a standard lid switch. Then
`logind` handles it like any laptop's, and what closing the lid does is `HandleLidSwitch=` in
`/etc/systemd/logind.conf` -- see `logind.conf(5)`. The Pi cannot suspend, so the useful values
are `ignore`, `lock` and `poweroff`.

It is not enabled by the package. The kernel then owns GPIO27, which `argononeupd` also takes,
so that daemon has to go -- and with it, the vendor's battery handling, which argond takes
over. One script does all of it, and records what it changed:

```sh
sudo /usr/libexec/argon-utils/oneup-takeover lock --full   # or ignore / poweroff; --full optional
sudo reboot
```

It disables `argononeupd`, installs the overlay into `/boot/firmware/overlays/` and adds
`dtoverlay=argon-oneup-lid` to `config.txt` (backing it up first), writes
`/etc/systemd/logind.conf.d/50-argon-oneup-lid.conf` with `HandleLidSwitch=`, and with `--full`
sets argond's mode to `full`, so it may power off on a critical battery.

Afterwards `busctl get-property org.freedesktop.login1 /org/freedesktop/login1
org.freedesktop.login1.Manager LidClosed` follows the lid. To undo exactly:

```sh
sudo /usr/libexec/argon-utils/oneup-restore
sudo reboot
```


## Building and installing the Debian package

The repo carries a native Debian source package in [`debian/`](../debian), so the normal
tooling applies:

```sh
dpkg-buildpackage -b -us -uc          # runs the test suite as part of the build
sudo apt install ../argon-utils_0.1.0_arm64.deb
```

`apt install` on a local file rather than `dpkg -i` so dependencies are resolved. The build
uses `--locked --offline`, so it builds from the committed `Cargo.lock` and cannot quietly pull
a different dependency version off the network, and it prefers a rustup toolchain because
`rust-toolchain.toml` pins 1.95.0 and Debian's `/usr/bin/cargo` ignores that file.

Where things land, and why:

| Path | Contents |
|---|---|
| `/usr/bin/argond`, `/usr/bin/argonctl` | the daemon and the CLI |
| `/usr/lib/systemd/system/argond.service` | the unit, enabled and started by dpkg |
| `/usr/lib/udev/rules.d/60-argon-utils.rules` | package-provided rules; `/etc/udev/rules.d` stays free for your overrides |
| `/usr/share/polkit-1/rules.d/50-argon-utils.rules` | likewise: `/etc/polkit-1/rules.d` takes precedence if you need to change it |
| `/etc/argon-utils/config.toml` | **conffile** -- your edits survive upgrades |
| `/etc/xdg/autostart/argon-notify-agent.desktop` | **conffile** -- the notification agent, from your next login |
| `/usr/bin/argon-tray`, `/etc/xdg/autostart/argon-tray.desktop` | the panel icon, from your next login (**conffile**) |

### Installing does not arm anything

The package ships `mode = "read-only"`, and `postinst` says so. In that mode `argond`
monitors the UPS, publishes `/run/argon-utils/ups.state` and logs what it *would* do, but it
changes no device state and will **not** power the machine off on a critical battery.

To arm the low-battery poweroff:

```sh
sudo sed -i 's/^mode = "read-only"/mode = "full"/' /etc/argon-utils/config.toml
sudo systemctl restart argond
journalctl -u argond -n 20 --no-pager     # expect: ups: shutdown ENABLED
```

Because the config is a conffile, that edit survives package upgrades -- and dpkg will ask
before touching it.

### What installation changes on the machine

Two things beyond dropping files in place, both in `postinst`:

- **It creates the `argon` system user** (no home, no shell). The daemon runs as that user and
  gets its device access from the `i2c`, `gpio` and `dialout` groups named in the unit. If any
  of those groups are missing, `postinst` warns rather than leaving you to decode a systemd
  failure.
- **It disables the vendor's UPS daemons** `argononeupsd` and `argonupsrtcd`, because two
  readers on one CDC-ACM port split the byte stream and both desynchronise mid-frame. This
  cannot be a dpkg `Conflicts`: the vendor stack is not a dpkg package, it is a curl-to-shell
  installer that writes into `/etc/argon`. Their enabled/active state is recorded in
  `/var/lib/argon-utils/retired-units` first, and `postrm` restores exactly that.

### Removing it

```sh
sudo apt remove argon-utils     # restores the vendor UPS daemons, keeps your config
sudo apt purge  argon-utils     # also removes the config, the argon user and the group
```

`postrm` warns if a poweroff is still scheduled when the package goes away. That is deliberate:
`argond` leaves a pending shutdown in place when it stops, because a critical battery does not
stop being critical, and cancelling it on your behalf could be the thing that lets the UPS cut
power without a clean shutdown.
