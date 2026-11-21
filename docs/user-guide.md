# CAN Bench user guide

- [Installing](#installing)
- [Connecting an adapter](#connecting-an-adapter)
- [CAN Terminal page](#can-terminal-page)
- [Periodic transmit](#periodic-transmit)
- [Device page](#device-page)
- [Firmware Update page](#firmware-update-page)
- [Keyboard and command line](#keyboard-and-command-line)
- [Where files are stored](#where-files-are-stored)
- [Troubleshooting](#troubleshooting)

---

## Installing

Download the file for your system from the [Releases](https://github.com/clawntl/can-bench/releases) page and unpack it.

| System | File | First start |
|---|---|---|
| Windows | `can-bench-…-windows-x64.zip` | Run `can-bench.exe` |
| Linux | `can-bench-…-linux-x64.tar.gz` | Install the udev rules (below), then run `./can-bench` |
| macOS (Apple Silicon / Intel) | `can-bench-…-macos-arm64.zip` / `…-macos-x64.zip` | The app is not notarised: right-click **CAN Bench.app → Open** the first time |

### Drivers and permissions

| | Candlelight firmware | Slcan firmware | Firmware update (DFU) |
|---|---|---|---|
| **Windows** | WinUSB (installed by the CANable firmware updater or [Zadig](https://zadig.akeo.ie)) | COM port, no driver | WinUSB for `0483:DF11` |
| **Linux** | udev rule. CAN Bench detaches the kernel `gs_usb` driver while it has the adapter open | `/dev/ttyACM*`: udev rule, or be in the `dialout` group | udev rule |
| **macOS** | none | `/dev/cu.usbmodem*` | none |

Linux udev rules (from the download, or [`packaging/linux/99-can-bench.rules`](../packaging/linux/99-can-bench.rules)):

```sh
sudo cp 99-can-bench.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
# then unplug and replug the adapter
```

## Connecting an adapter

1. Plug in the CANable 2.5 adapter and click **⟳ Refresh** if it is not listed.
2. Set the **nominal baudrate and sample point** (for CAN FD also the data rate). CAN Bench calculates
   the bit timing for the adapter's clock; if the exact sample point is not possible it tells you
   what it uses.
3. Choose the **mode**: *Normal*, *Silent* (listen only, no ACK — safe for sniffing a running bus),
   *Loopback internal* (no bus needed) or *Loopback external*.
4. Click **▶ Open**. The adapter's details (board, firmware, timing) appear in purple in the trace.

Settings are locked while the adapter is open; close it to change them.

## CAN Terminal page

![CAN Terminal with the periodic list and decoded trace](images/terminal-light.png)

**Trace colours:** blue = sent, green = received, olive = sent and acknowledged by another node
(echo), purple = adapter information, orange/red = warnings and bus errors.

- **Decoded frames.** With a [device profile](#device-page) loaded, frames are followed by
  `⇒ message n: signal=value …`. Turn this off with *Decode in trace* on the Device page.
- **Live ID table** shows one row per ID with the latest data, count and measured period.
- **Hide repeats** shows each identical frame only once. **Pause** freezes the trace (counters keep running).
- **Start logging** writes everything to a text file as it happens; **Save trace** saves what is on screen.
- **Comment** inserts a marker line, handy for "motor started here" notes in a log.
- **Host filter**: up to 8 `ID,mask` pairs; the adapter then only passes matching IDs to the PC.
- **Slcan command** (Slcan adapters only): send raw Slcan text commands.

**Sending:** type the ID in hex (3 digits = 11-bit, more = 29-bit) and the data bytes, then **Send** or Enter.

## Periodic transmit

Click **⏱ Periodic** in the Send bar to open the list.

- **➕ Add row from Send bar** copies the current ID/data/flags into a new row.
- Tick **On** to start a row. Each row has its own interval (1 ms – 60 s) and counts what it sent.
- **⬆** copies a row back into the Send bar; **✖** deletes it.
- **Esc stops every row** (and every Device output), wherever you are in the window.
- Timing runs in the adapter thread, not in the window, so it keeps going while CAN Bench is minimised.
  With Slcan adapters expect a few ms of jitter, with Candlelight about 1 ms.
- The list is saved automatically. Rows always start **off** after CAN Bench restarts.

## Device page

![Device page with two BLDC-Driver nodes](images/device-dark.png)

Load a **device profile** (a `.toml` file describing your protocol — see the
[profile reference](../profiles/README.md)) with **📂 Load…**. CAN Bench remembers it for next time.
**⟳ Reload** re-reads the file after you edit it.

- **Nodes** — one row per node that has been heard, with alive/missing state and the columns the
  profile lists. Alarm values and missing nodes are red. Click a node number to select it for commands.
- **Outputs** — sliders that are sent periodically (for example a motor current). Tick the nodes to
  drive, press **▶ Off** to switch it on. Switching it off or pressing **Esc** sends the profile's
  `stop` values once (for example 0 A). Outputs never restart on their own.
- **Commands** — one button per command in the profile, sent to the node chosen above. Commands
  marked `confirm` ask first. Replies (or refusals / timeouts) appear under **Results** and in the trace.
- **Signals** — every decoded value with its age. Sent values are blue. Tick a value to plot it.
- **Plot** — *Follow* shows the last N seconds; untick it to drag and zoom freely. **💾 CSV** saves the
  history of the plotted signals (the last 10 minutes, up to 20 000 points per signal).

**No hardware?** Start CAN Bench with `--demo`: the adapter list gets a *Demo bus* with two simulated
BLDC-Driver boards. Open it and load `profiles/bldc-driver.toml`.

## Firmware Update page

![Firmware Update page](images/firmware.png)

1. **Adapter:** pick the adapter and click **Enter DFU mode**. (Already in DFU mode, e.g. booted with
   the BOOT button? It is listed as *STM32 bootloader*.)
2. **Firmware file:** click **Get latest from GitHub**, or **➕ Add file…** for a `.dfu`/`.bin`.
   Files that match your adapter's board are marked ✔. Slcan = COM port, Candlelight = USB.
3. Click **⚡ Flash firmware**. The flash is erased, written and verified, then the adapter restarts.

The STM32 ROM bootloader cannot be overwritten, so a failed update can simply be repeated.
**Back up firmware** reads the current flash into a file first, if you want to be able to go back.
**Disable pin BOOT0** stops boards with a floating BOOT0 pin from starting in DFU mode by themselves.

## Keyboard and command line

| Key | Action |
|---|---|
| **Esc** | Stop all periodic rows and Device outputs (outputs send their stop values) |
| **Enter** | In the Send bar / Slcan box / Comment box: send |

| Option | Meaning |
|---|---|
| `--demo` | Add the simulated *Demo bus* adapter |
| `--profile FILE` | Load a device profile at start |
| `--open` | Open the selected adapter at start |
| `--device`, `--firmware`, `--periodic` | Start on the Device page / Firmware page / with the periodic list open |
| `--dark` | Dark colours for this run |
| `--screenshot FILE [--shot-delay MS]` | Save a screenshot after MS milliseconds and quit (used for these docs) |
| `--autopilot` | Scripted demo session (with `--demo --profile profiles/bldc-driver.toml --open`) |

## Where files are stored

| | Folder |
|---|---|
| Windows | `%APPDATA%\CanBench` |
| macOS | `~/Library/Application Support/CanBench` |
| Linux | `$XDG_DATA_HOME/CanBench` or `~/.local/share/CanBench` |

It holds `Firmware/` (downloaded and added firmware files), `periodic.json` (the periodic list),
`profile.txt` (the last device profile) and `theme.txt`.

## Troubleshooting

| Problem | Try |
|---|---|
| Adapter not listed | Click ⟳ Refresh. Windows: Candlelight needs the WinUSB driver. Linux: install the udev rules and replug. |
| "Cannot claim the adapter" | Another program (or another CAN Bench window) has it open. On Linux, check nothing else uses `can0`. |
| Frames are sent but never acknowledged (no olive echo) | No other node on the bus, wrong baudrate, missing 120 Ω termination, or the adapter is in Silent mode. |
| "Bus off" / error passive | Wrong baudrate or sample point, missing termination, CAN-H/CAN-L swapped. |
| Profile does not load | The red message names the message, signal and field. Unknown keys are errors — check spelling. |
| A node shows *missing* | Its heartbeat message did not arrive within `timeout_ms`; check `[nodes].heartbeat` names the right message. |
| Periodic frames stall | The adapter's Tx buffer is full: no node is acknowledging, or the bus is overloaded (see Busload). |
