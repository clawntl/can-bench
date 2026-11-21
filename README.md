# CAN Bench

**A fast, native CAN bus terminal, device dashboard and firmware updater for
[CANable 2.5](https://github.com/Elmue/CANable-2.5-firmware-Slcan-and-Candlelight) adapters —
built for robotics and embedded development.** Windows, Linux and macOS. Written in Rust.

![CAN Bench Device page: two motor driver nodes, current sliders and a live velocity plot](docs/images/device-dark.png)

<sub>The Device page driving two <a href="https://github.com/clawntl/BLDC-Driver">BLDC-Driver</a>
boards (here the built-in simulator, `--demo`): node table, motor-current sliders, command buttons
and a live velocity plot — all described by one <a href="profiles/bldc-driver.toml">TOML file</a>.</sub>

## Features

**CAN terminal**
- Works with both CANable 2.5 firmwares: **Candlelight** (USB) and **Slcan** (serial port)
- Classic CAN and **CAN FD**, any baudrate, automatic bit timing for the sample point you want
- Normal, silent (sniff), internal and external loopback modes; software 120 Ω termination on boards that have it
- Colour-coded trace with ACK echo, live ID table with periods, busload, decoded bus errors
- Host-side ID filters, logging to file, comments, raw Slcan commands

**Periodic transmit**
- A saved list of frames, each with its own interval — timed in the adapter thread, so it keeps
  running while the window is minimised
- **Esc stops everything**

**Device page (your protocol, no code)**
- Describe your messages in a [device profile](profiles/README.md) and get **decoded signals**,
  a **node table** with alive/missing and alarms, **live plots** with CSV export,
  **sliders** that are sent periodically, and **command buttons** that wait for the reply
- Handles node-numbered IDs (`0x100 + n`), command-byte multiplexing, several nodes in one frame,
  big/little-endian integers and floats, enums, flags and bit fields

**Firmware updater**
- Download the latest CANable 2.5 firmware from GitHub and flash it over USB DFU, with verify
- Enter/leave DFU mode from the app, back up the current firmware, disable a floating BOOT0 pin

<table>
<tr>
<td width="50%"><img src="docs/images/terminal-light.png" alt="CAN Terminal page with periodic list and decoded trace"></td>
<td width="50%"><img src="docs/images/firmware.png" alt="Firmware Update page"></td>
</tr>
<tr>
<td align="center"><sub>CAN Terminal: periodic list and a decoded trace</sub></td>
<td align="center"><sub>Firmware Update</sub></td>
</tr>
</table>

## Quick start

1. Download CAN Bench for your system from the
   **[Releases](https://github.com/clawntl/can-bench/releases)** page.
   (Linux: install the [udev rules](packaging/linux/99-can-bench.rules). macOS: right-click → Open the first time.)
2. Plug in your CANable, pick the baudrate, click **▶ Open**.
3. For your own protocol, write a [device profile](profiles/README.md) and load it on the **Device** page.

**No adapter at hand?** Run `can-bench --demo`, open the *Demo bus*, and load
[`profiles/bldc-driver.toml`](profiles/bldc-driver.toml) on the Device page.

## Documentation

| | |
|---|---|
| 📘 [User guide](docs/user-guide.md) | Every page and option, drivers per platform, troubleshooting |
| 🧩 [Device profile reference](profiles/README.md) | The TOML format, with recipes |
| 🛠 [Development notes](docs/development.md) | Code layout, threads, tests, screenshots, releases |
| 🔌 [Adapter protocol notes](app/MECHANICS.md) | Candlelight/Slcan/DFU details the adapter code relies on |

## A device profile in 10 lines

```toml
[nodes]
range = [1, 15]
heartbeat = "telemetry.status"

[[message]]
name = "telemetry"
id = "0x100 + n"                  # one ID per node
mux = { byte = 0 }                # byte 0 says what the frame is
  [[message.variant]]
  mux = 0x03
  name = "status"
  signals = [
    { name = "state",  byte = 1, type = "u8", enum = { 1 = "IDLE", 2 = "RUNNING", 3 = "FAULT" }, alarm = ["FAULT"] },
    { name = "supply", byte = 3, type = "u16be", scale = 0.001, unit = "V" },
  ]
```

The trace then reads `101: 03 02 00 5D 6F …  ⇒ status n1: state=RUNNING supply=23.919 V`,
node 1 appears in the node table, and both values can be plotted. See the
[full BLDC-Driver profile](profiles/bldc-driver.toml) for outputs and commands.

## Platform support

| | Windows 10/11 | Linux (x64) | macOS 11+ (Apple Silicon, Intel) |
|---|---|---|---|
| Candlelight (USB) | ✔ WinUSB driver | ✔ udev rule; kernel `gs_usb` is detached while open | ✔ no driver |
| Slcan (serial) | ✔ | ✔ `/dev/ttyACM*` | ✔ `/dev/cu.usbmodem*` |
| Firmware update | ✔ | ✔ | ✔ |

All four builds are compiled and unit-tested on every push by
[GitHub Actions](.github/workflows/build.yml). The Linux and macOS builds have not been tested with real adapters yet — reports welcome.

## Building from source

```sh
cd app
cargo run --release
```

Requires a recent stable Rust. On Linux also install
`pkg-config libudev-dev libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev`.
Releases are made by pushing a `v*` tag — see [development notes](docs/development.md#releases).

## Credits

- CANable 2.5 firmware and its protocol: [ElmüSoft](https://github.com/Elmue/CANable-2.5-firmware-Slcan-and-Candlelight)
- GUI: [egui / eframe](https://github.com/emilk/egui) and [egui_plot](https://github.com/emilk/egui_plot); USB: [nusb](https://github.com/kevinmehall/nusb)
