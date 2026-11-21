# CAN Bench

A Rust + egui desktop CAN analyser terminal and firmware updater for
[CANable 2.5](https://github.com/Elmue/CANable-2.5-firmware-Slcan-and-Candlelight)
adapters (Slcan and Candlelight firmware), aimed at development and robotics use.

## Features
- Auto-detects CANable 2.5 adapters (Candlelight over USB / Slcan over USB CDC)
- Classic CAN and CAN FD, automatic bit-timing search (baud + sample point)
- Normal, listen-only, internal and external loopback modes
- Trace view and live ID table (count + repeat period), busload, bus error decoding
- **Periodic transmit**: a saved list of frames, each with its own interval. Timing runs in the
  adapter thread, so it keeps going while the window is minimised. **Esc stops everything.**
- Host filter, raw Slcan commands, logging and trace save
- Firmware updater: enter DFU, flash `.dfu`/`.bin` via DfuSe, fetch latest firmware from GitHub, disable BOOT0
- Light/dark theme

## Download
Builds for Windows, Linux and macOS are attached to each
[release](../../releases). Every push also uploads builds as workflow artifacts on the Actions tab.

To make a release, push a version tag:
```
git tag v0.2.0
git push origin v0.2.0
```

## Platform notes
| | Candlelight firmware | Slcan firmware | Firmware update (DFU) |
|---|---|---|---|
| **Windows** | WinUSB driver (installed by the CANable firmware updater / Zadig) | COM port | WinUSB driver for `0483:DF11` |
| **Linux** | Install [the udev rules](packaging/linux/99-can-bench.rules). The kernel `gs_usb` driver is detached while CAN Bench has the adapter open | `/dev/ttyACM*` (udev rule, or be in the `dialout` group) | udev rule |
| **macOS** | No driver needed | `/dev/cu.usbmodem*` | No driver needed |

macOS builds are ad-hoc signed, not notarised: the first time, right-click the app → **Open**.

Settings and the periodic list are stored in `%APPDATA%\CanBench` (Windows),
`~/Library/Application Support/CanBench` (macOS) or `~/.local/share/CanBench` (Linux).

## Build from source
```
cd app
cargo run --release
```
Linux needs: `pkg-config libudev-dev libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev`.

See [app/MECHANICS.md](app/MECHANICS.md) for the protocol details the app implements.
