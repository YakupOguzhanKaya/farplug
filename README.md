# farplug

Share the USB devices plugged into a Linux box with other machines on your
network. On the client, a device shows up as if it were plugged in locally:
`lsusb` lists it and the client's own drivers handle it.

It speaks the standard [USB/IP] protocol and lets the Linux kernel move the
USB traffic (`usbip-host` on the server, `vhci_hcd` on the client). farplug
adds what the stock `usbip` tools leave to you:

- **Devices are chosen by VID/PID/serial or physical port**, not by bus number,
  and exported under a name you pick (`zigbee`, `printer`).
- **Hotplug on the server.** A configured device is exported as soon as it is
  plugged in and re-exported after a replug or a reboot. Unlisted devices are
  never touched.
- **Clients reconnect on their own.** `farplug attach` keeps devices attached
  through server restarts, network drops and replugs.

Stopping `farplugd` stops sharing: it gives every device back to the server,
which disconnects its client. After a restart, clients re-attach on their own
within a few seconds.

[USB/IP]: https://docs.kernel.org/usb/usbip_protocol.html

## Status

| | Server | Client |
|---|---|---|
| Linux | working | working |
| Windows | — | written, not yet tested on Windows (x64 and ARM64, uses [usbip-win2]) |
| macOS | — | serial adapters and keyboards/mice only (see below), not yet tested on a Mac |

On Windows, farplug drives the [usbip-win2] kernel driver, which is signed
by Microsoft. Install usbip-win2 first; it was written against 0.9.8.1.

macOS has no USB/IP driver, and a virtual USB controller needs an
entitlement only Apple grants, so the macOS client drives devices itself:

- **Serial adapters** (CDC-ACM, CP210x, CH34x, FTDI; most Zigbee and Z-Wave
  sticks) become a serial port linked at `~/.farplug/serial/<id>`. Programs
  set the baud rate as usual. DTR and RTS stay on, so boards that reset on a
  DTR toggle, like some Arduinos, do not reset.
- **Keyboards and mice** type and move the pointer on the Mac. This needs the
  Accessibility permission (System Settings > Privacy & Security >
  Accessibility); attaching asks for it. Apps see input events, not a device,
  so vendor tools and raw-input games do not see it.
- Other devices are refused with a message saying so.

[usbip-win2]: https://github.com/vadimgrn/usbip-win2

## Install

Prebuilt static binaries, no Rust toolchain required.

**Server** (Linux x86-64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/YakupOguzhanKaya/farplug/master/install/install-server.sh | sh
```

**Client** (Linux x86-64/arm64, macOS):

```sh
curl -fsSL https://raw.githubusercontent.com/YakupOguzhanKaya/farplug/master/install/install-client.sh | sh
```

**Client on Windows** (x64 or ARM64): the `curl … | sh` line above works from
Git Bash. Without Git Bash, run this in PowerShell instead:

```powershell
irm https://raw.githubusercontent.com/YakupOguzhanKaya/farplug/master/install/install-client.ps1 | iex
```

It installs per user, without admin rights, into `%LOCALAPPDATA%\Programs\farplug`.
It adds `farplug` to your `PATH`, adds a Start menu entry, and starts the tray
icon at every login. If the [usbip-win2] driver is missing, it installs it,
which needs one administrator approval.

Both sides need the USB/IP kernel modules: `usbip-host` on the server and
`vhci-hcd` on the client. Most distros ship them. On Debian or Ubuntu cloud
images they are in `linux-modules-extra-$(uname -r)`.

**Uninstall** with the same command plus `--uninstall`. Devices are detached
(on a client) or given back to the machine (on the server) first. The server
config and the client's saved servers and devices stay unless you also pass
`--purge`:

```sh
curl -fsSL .../install-server.sh | sh -s -- --uninstall [--purge]
curl -fsSL .../install-client.sh | sh -s -- --uninstall [--purge]
```

In PowerShell: `& ([scriptblock]::Create((irm <install-client.ps1 url>))) -Uninstall`.
The usbip-win2 driver stays installed; remove it under Settings > Apps.

## Set up

On the server:

```sh
sudo farplugd add      # pick plugged-in devices to export
sudo farplugd check    # what each config entry matches
```

`add` writes entries like the one below to `/etc/farplug/config.toml`; you can
also edit the file by hand. The running server applies changes within a
second: unchanged exports stay connected, removed ones go back to the host,
and an invalid edit is ignored with a warning in the log. Set
`export_all = true` to export everything plugged in without listing it; see
`config.example.toml`.

```toml
[device.zigbee]
vid = "10c4"
pid = "ea60"
serial = "0001"
```

On a client, the `farplug-agent` service owns the attachments. It keeps
devices attached through disconnects and re-attaches them after a reboot,
until you detach them. The CLI only gives it orders:

```sh
farplug --server usbgw.local list
farplug --server usbgw.local attach zigbee
farplug status
farplug detach zigbee
farplug alias usbgw.local garage   # then: farplug --server garage list
```

Or use the GUI: `farplug-gui` opens a window listing your servers and their
devices, with an Attach/Detach button for each. `farplug-gui --tray` puts an
icon in the system tray with a checkbox per device; its color is green when
everything is attached, amber while something is reconnecting, and grey when
nothing is attached. Clicking the icon (double-clicking on Windows) opens the
window, and only one window opens at a time. The installer sets the tray icon
to start at login on Linux and Windows desktops. On Linux it needs a panel
with StatusNotifierItem support: Waybar, KDE, XFCE, or GNOME with the
AppIndicator extension. The GUI is only a remote: closing it leaves devices
attached.

On Windows there is no service. The agent runs in your session, and the GUI
starts it if it is not running.

On Linux, members of the `farplug` group can control the agent without
`sudo`; the client installer adds you, and it takes effect right away. `farplug attach --foreground` attaches
without the agent, until Ctrl-C, which needs root on Linux.

The stock tools also work against the server:
`usbip attach -r usbgw.local -b zigbee`.

To give a device back to the server's own drivers, remove it from the config
and run `sudo farplugd release zigbee`.

## Known limits

- **No encryption or authentication**, by design. It is meant for a trusted
  home network. To reach it from elsewhere, use Tailscale or WireGuard. Do not
  expose port 3240 to the internet.
- **One client per device.** A second import is refused while the device is in
  use.
- Hubs are never exported, only the devices behind them.

## Build and test

```sh
cargo build --release      # target/release/farplugd, target/release/farplug
cargo test
tests/run-e2e.sh           # end-to-end, in a privileged Docker container
```

`tests/gui-screenshot.sh` renders the window under Xvfb for a visual check, and
`tests/tray-sni.sh` drives the Linux tray over D-Bus, both in the same
container.

The end-to-end test attaches an emulated device through the real `vhci_hcd`
driver of Docker's Linux VM. It also checks the server against the stock
`usbip` tool. Docker Desktop's kernel has no `usbip-host`, so the server's
binding to real hardware is only exercised on a real Linux machine.

Releases are cut by pushing a tag: `git tag v0.2.0 && git push origin v0.2.0`.

## Licence

MIT.
