# usbridge

Share USB devices from a small always-on box to any machine on your LAN,
without kernel drivers on the client.

Unlike USB/IP, `usbridge` does not try to make a remote device appear as a
generic local USB device. That approach needs a virtual USB host controller in
the kernel, which is why USB/IP has no usable macOS client — Apple does not
grant the entitlement. Instead `usbridge` bridges at the **device-class** level,
so every client is plain userspace and works the same on Linux and macOS.

## Status

| Profile | Server | Client | Notes |
|---|---|---|---|
| Serial (CDC-ACM / FTDI / CP210x) | working | working | Zigbee, Z-Wave, UPS, ESP, GPS, printers |
| libusb shim | planned | planned | SDR, Coral TPU, `dfu-util`, `esptool` |
| Block storage | planned | — | will export over NBD rather than tunnel USB |
| HID | planned | planned | uinput on Linux, Karabiner VHID on macOS |

Only the serial profile exists today. It covers most of a typical home rack.

## How it works

```
  ┌──────────── gateway (Lenovo mini / Pi) ────────────┐
  │  usbridged                                         │
  │    config.toml ──► match VID/PID/serial ──► /dev/ttyUSB*
  └──────────────────────┬─────────────────────────────┘
                         │  TCP 7070, length-delimited postcard frames
  ┌──────────────────────▼─────────────────────────────┐
  │  usbridge attach zigbee --link ~/.usbridge/dev/zigbee
  │    └─► openpty() ─► /dev/ttys004 ◄── symlink        │
  └────────────────────────────────────────────────────┘
            applications open the symlink as a normal serial port
```

Devices are matched by USB VID/PID/serial, never by `/dev` path, so a replug or
reboot that renumbers the ports does not break anything. The client's pty
outlives individual TCP sessions: if the gateway reboots or the device is
unplugged, the pty stays open and the application holding it does not need to be
restarted.

## Build

```sh
cargo build --release
# gateway binary:  target/release/usbridged
# client binary:   target/release/usbridge
```

Cross-compile for a Pi:

```sh
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu -p usbridged
```

## Run

On the gateway:

```sh
sudo install -D -m0755 target/release/usbridged /usr/local/bin/usbridged
sudo install -D -m0644 config.example.toml /etc/usbridge/config.toml
sudo useradd --system --no-create-home usbridge
sudo install -D -m0644 dist/usbridged.service /etc/systemd/system/usbridged.service
sudo systemctl enable --now usbridged
```

On a client:

```sh
usbridge --server usbgw.local list
usbridge --server usbgw.local attach zigbee --link ~/.usbridge/dev/zigbee
```

Then point Home Assistant, `zigbee2mqtt`, `nut` or anything else at the symlink.

## Known limits

- **Line settings are set at attach time.** A pty cannot report termios changes
  the way a real USB serial adapter does, so an app that reconfigures baud rate
  mid-stream is not followed. Set it in `config.toml`, or pass `--baud`.
- **DTR/RTS are wired in the protocol but not driven from the pty.** This blocks
  auto-reset flashing (`esptool`, Arduino) over the bridge. Needs pty packet
  mode on the client side — see `ClientMsg::SetControlLines`.
- **No authentication or encryption.** Run it on a trusted VLAN, or put it
  behind WireGuard. Do not expose port 7070 to the internet.
- **The gateway must run on Linux.** It builds and runs on macOS, but opening a
  pty-backed device there fails with ENOTTY, so the test suite and the fake-device
  workflow are Linux-only.
- **One client per device.** A second client is refused unless it passes
  `--steal`, which evicts the holder.

## Licence

MIT.

## Test

```sh
tests/run-smoke.sh
```
