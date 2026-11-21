# Development notes

## Building

```sh
cd app
cargo run              # debug build
cargo run --release    # optimised
cargo test             # unit tests (bit timing, Slcan parsing, DfuSe files, periodic scheduler, profiles)
```

Linux needs `pkg-config libudev-dev libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev`.

## Layout

```
app/src/
  main.rs        window setup, command-line flags
  app.rs         the window: CAN Terminal + Firmware pages, periodic list UI
  app/devtab.rs  the Device page: profile state, node table, outputs, commands, signals, plot
  can.rs         CanFrame, hex parsing, host filters
  periodic.rs    TxSpec (what the user typed), the saved periodic list, the Scheduler
  profile.rs     device profiles: TOML schema, expressions, decoder, encoder, command builder
  device.rs      adapter discovery and the worker threads (Candlelight, Slcan, demo, firmware tasks)
  candle.rs      Candlelight / gs_usb + ElmüSoft protocol over USB (nusb)
  slcan.rs       Slcan ASCII protocol over the serial port
  timing.rs      bit-timing search (baud + sample point)
  dfu.rs         DfuSe flashing through the STM32 ROM bootloader
  dfufile.rs     .dfu file parser
  sim.rs         the --demo bus: two simulated BLDC-Driver nodes
profiles/        device profiles and the profile format reference
packaging/       udev rules (Linux), Info.plist (macOS .app)
docs/            user guide, these notes, screenshots
app/MECHANICS.md protocol constants the adapter code relies on, with sources
```

## Threads

The window never talks to USB directly. Opening an adapter starts one worker thread
(`device::open`) that owns the connection:

```
 window (egui)                     adapter thread
 ─────────────                     ──────────────
 Cmd::Send(frame)          ───▶    write to adapter
 Cmd::SetPeriodic(jobs)    ───▶    Scheduler: sends due frames itself
 Cmd::Identify / Raw / Close ─▶
                           ◀───    Ev::Rx / Tx(frame, tag) / Echo / Error / Busload / Opened / Closed
```

Periodic rows and Device outputs are both just `Job`s in that thread's `Scheduler`, so their timing
does not depend on the window repainting (which stops while it is minimised). The `tag` on each
`Ev::Tx` tells the window which row or output sent the frame (outputs use tags from `OUTPUT_TAG`).
`Scheduler::set` keeps a job's phase when only its data changes (dragging a slider), and skips
missed slots instead of sending a burst after a stall.

Decoding happens in the window thread (`DevState::on_frame`), for received and sent frames.

## Device profiles

`profile.rs` compiles the TOML once on load: expressions are parsed, types checked, and an index
`(ext, id) → [(message, node)]` is built by evaluating every message's ID for every node in
`[nodes].range`. Decoding a frame is then one hash lookup; a frame shared by several nodes
(BLDC-Driver's current frame) decodes once per node. `encode_output` does the reverse, grouping
driven nodes by ID and filling undriven nodes' slots with the signal `default`.

The format reference is [`profiles/README.md`](../profiles/README.md); keep it in step with the
`*Def` structs in `profile.rs`. `profiles/bldc-driver.toml` is loaded by a unit test, so a broken
example fails `cargo test`.

## Screenshots

The images in `docs/images` are made by the app itself on the demo bus:

```sh
cd app
cargo build
P=../profiles/bldc-driver.toml
./target/debug/can-bench --demo --open --autopilot --profile $P --device --dark --shot-delay 6500 --screenshot ../docs/images/device-dark.png
./target/debug/can-bench --demo --open --autopilot --profile $P --device        --shot-delay 6500 --screenshot ../docs/images/device-light.png
./target/debug/can-bench --demo --open --autopilot --profile $P --periodic      --shot-delay 4000 --screenshot ../docs/images/terminal-light.png
./target/debug/can-bench --firmware --shot-delay 2500 --screenshot ../docs/images/firmware.png
```

Point `APPDATA` (Windows) / `HOME` at an empty folder first if you do not want your own periodic
list, theme or firmware folder paths in the pictures.

## Releases

[`.github/workflows/build.yml`](../.github/workflows/build.yml) builds and tests on Windows, Linux
(Ubuntu 22.04, for an older glibc), macOS arm64 and macOS x64 for every push and pull request, and
uploads the packages as workflow artifacts. Pushing a tag `v*` also creates a GitHub Release:

```sh
git tag v0.2.0
git push origin v0.2.0
```

Remember to bump `version` in `app/Cargo.toml` first. macOS builds are ad-hoc signed only (no
Developer ID / notarisation), so users have to right-click → Open the first time.
