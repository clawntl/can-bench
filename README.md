# CAN Bench

A Rust + egui desktop CAN analyser terminal and firmware updater for
[CANable 2.5](https://github.com/Elmue/CANable-2.5-firmware-Slcan-and-Candlelight)
adapters (Slcan and Candlelight firmware), aimed at development and robotics use.

## Features
- Auto-detects CANable 2.5 adapters (Candlelight over WinUSB / Slcan over USB CDC)
- Classic CAN and CAN FD, automatic bit-timing search (baud + sample point)
- Normal, listen-only, internal and external loopback modes
- Trace view and live ID table (count + repeat period), busload, bus error decoding
- Host filter, raw Slcan commands, logging and trace save
- Firmware updater: enter DFU, flash `.dfu`/`.bin` via DfuSe, fetch latest firmware from GitHub, disable BOOT0
- Light/dark theme

## Build
```
cd app
cargo run --release
```

See [app/MECHANICS.md](app/MECHANICS.md) for the protocol details the app implements.
