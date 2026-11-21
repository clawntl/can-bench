# Device profiles

A device profile is a [TOML](https://toml.io) file that describes **your** CAN protocol: which IDs
carry which values, how your boards are numbered, which commands they understand and which values
you want to drive with sliders. Load it on the **Device** page. CAN Bench has no project-specific
code: everything on that page comes from the profile.

| File | What it is |
|---|---|
| [`bldc-driver.toml`](bldc-driver.toml) | Complete profile for [BLDC-Driver](https://github.com/clawntl/BLDC-Driver) boards (protocol 4). A good template. |

**Try it without hardware:** start CAN Bench with `--demo`, pick *Demo bus* on the CAN Terminal
page, click **Open**, then load `bldc-driver.toml` on the Device page. Two simulated boards answer.

**Editing workflow:** keep the file open in your editor and click **⟳ Reload** on the Device page
after each save. Mistakes are reported at the top of the page with the line and the field, e.g.
`message "telemetry": variant "status": supply: unknown type 'u16'`. Unknown field names are
errors too, so typos do not go unnoticed.

---

## Contents

1. [A minimal profile](#a-minimal-profile)
2. [Expressions and the node number `n`](#expressions-and-the-node-number-n)
3. [`[nodes]`](#nodes)
4. [`[[message]]`](#message)
5. [Signals](#signals)
6. [`[[output]]` — sliders sent periodically](#output--sliders-sent-periodically)
7. [`[[command]]` — buttons with replies](#command--buttons-with-replies)
8. [Recipes](#recipes)

---

## A minimal profile

```toml
name = "Battery monitor"

[[message]]
name = "battery"
id = 0x351
signals = [
  { name = "voltage", byte = 0, type = "u16le", scale = 0.01, unit = "V", alarm_below = 44.0 },
  { name = "current", byte = 2, type = "i16le", scale = 0.1,  unit = "A" },
  { name = "soc",     byte = 4, type = "u8", unit = "%" },
]
```

Every frame with ID `0x351` now shows `battery: voltage=52.31 V current=-3.2 A soc=87 %` in the trace,
and the three values appear in the Signals list on the Device page, ready to plot.

## Expressions and the node number `n`

Where a number is expected for an **ID** or a **byte position**, you can also write an expression
in quotes. It may use:

- whole numbers, decimal or hex (`16`, `0x10`)
- `n`, the node number (needs a [`[nodes]`](#nodes) section)
- `+ - * / %` and parentheses. `/` is integer division, `%` is the remainder.

```toml
id   = "0x100 + n"               # node 1 -> 0x101, node 2 -> 0x102 …
id   = "0x010 + (n - 1) / 2"     # nodes 1 and 2 share 0x010, nodes 3 and 4 share 0x011 …
byte = "((n - 1) % 2) * 4"       # odd nodes use bytes 0-3, even nodes bytes 4-7
```

A message whose `id` uses `n` exists once per node. CAN Bench works out every node's ID from
`[nodes].range`, so a received frame tells it which node sent it.

## `[nodes]`

Only needed if your IDs depend on a node number.

```toml
[nodes]
range = [1, 15]                     # node numbers that can exist
label = "Node"                      # what to call them in the UI (default "Node")
heartbeat = "telemetry.status"      # the message (or message.variant) that means "alive"
timeout_ms = 500                    # shown as missing after this long without a heartbeat (default 1000)
columns = ["status.state", "status.supply", "velocity.velocity"]   # signals shown in the node table
```

| Key | Default | Meaning |
|---|---|---|
| `range` | required | First and last node number |
| `label` | `"Node"` | Name used in the UI |
| `heartbeat` | any message | Which received message marks a node as alive. Without it, any per-node message does |
| `timeout_ms` | `1000` | After this long without a heartbeat the node is shown as missing (red) |
| `columns` | none | Signal keys (see [Signals](#signals)) shown as columns in the node table |

## `[[message]]`

One entry per CAN message (or per family of messages, if the ID uses `n`).

```toml
[[message]]
name = "telemetry"
id = "0x100 + n"
ext = false          # true for 29-bit IDs
mux = { byte = 0 }   # byte 0 says which variant this frame is

  [[message.variant]]
  mux = 0x02
  name = "velocity"
  signals = [{ name = "velocity", byte = 1, type = "f32be", unit = "rad/s" }]

  [[message.variant]]
  mux = 0x03
  name = "status"
  signals = [ … ]
```

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Used in the trace and in signal keys |
| `id` | required | Number or [expression](#expressions-and-the-node-number-n) |
| `ext` | `false` | 29-bit identifier |
| `signals` | `[]` | The [signals](#signals) of a message without variants |
| `mux = { byte = N }` | none | Byte N selects a variant (a "command byte" or "multiplexer") |
| `[[message.variant]]` | none | `mux` = selector value, `name`, `signals` |
| `length` | `8` | Frame length when CAN Bench **sends** this message (outputs) |
| `fill` | `0` | Value of bytes no signal writes when sending |

Messages are decoded in both directions: frames you send are decoded too, so you can plot what you
commanded next to what the board measured.

## Signals

```toml
{ name = "supply", byte = 3, type = "u16be", scale = 0.001, unit = "V", invalid = 0, alarm_below = 10.0 }
```

The **key** of a signal is `variant.signal` (or `message.signal` without variants), e.g.
`status.supply`. Keys are used by `[nodes].columns` and shown in the Signals list.

**Value = raw × `scale` + `offset`.**

| Key | Default | Meaning |
|---|---|---|
| `name` | required | |
| `byte` | required | First byte. Number or expression (may use `n`) |
| `type` | required | See the table below |
| `bit`, `bits` | none | Take only `bits` bits starting at bit `bit` (0 = least significant) of an integer type |
| `scale`, `offset` | `1`, `0` | Physical value = raw × scale + offset |
| `unit` | none | Shown after the value |
| `decimals` | auto | Digits after the point (auto: 3 for floats and fractional scales, else 0) |
| `enum` | none | `{ 1 = "IDLE", 2 = "RUNNING" }`: show names instead of numbers |
| `flags` | none | `{ 0 = "TIMEOUT", 3 = "FAULT" }`: bit number → name, shown as `TIMEOUT\|FAULT` |
| `invalid` | none | Raw value that means "no reading" (shown as –, not plotted). Matches the signed value or the bit pattern, so `0x8000` works for an `i16` |
| `alarm` | `[]` | Enum or flag names that are alarms (shown red, node row turns red) |
| `alarm_below`, `alarm_above` | none | Numeric alarm limits (after scaling) |
| `min`, `max` | ±100 | Slider range when the message is an [output](#output--sliders-sent-periodically) |
| `default` | none | Value sent for nodes an output does **not** drive (e.g. `nan` = "no command") |
| `stop` | none | Value sent once when an output is switched off or Esc is pressed |

A signal that does not fit in a received frame (a shorter frame) is skipped, not shown as 0.
A float that is NaN is shown as `NaN` and not plotted.

**Types:** add `be` (big-endian, Motorola) or `le` (little-endian, Intel) to every multi-byte type.

| Type | Size | |
|---|---|---|
| `u8`, `i8` | 1 | unsigned / signed |
| `u16be` `u16le` `i16be` `i16le` | 2 | |
| `u24be` `u24le` | 3 | |
| `u32be` `u32le` `i32be` `i32le` | 4 | |
| `u64be` `u64le` `i64be` `i64le` | 8 | |
| `f32be` `f32le` | 4 | IEEE 754 float |
| `f64be` `f64le` | 8 | IEEE 754 double |

## `[[output]]` — sliders sent periodically

An output sends a message every `period_ms`, with each signal set from a slider.

```toml
[[output]]
name = "Motor current"
message = "current_cmd"     # a message without variants
period_ms = 10              # default 100
```

On the Device page each output has an **On/Off** button, a **Zero** button, a checkbox per node
("drive this node") and a slider per signal (range = the signal's `min`/`max`).

- The frames are timed in the adapter thread, like the periodic list, so they keep going while the
  window is minimised.
- If several nodes share one frame (see `id = "0x010 + (n - 1) / 2"` above), CAN Bench builds one
  frame per ID. Nodes you do not drive get each signal's `default` (BLDC-Driver: `nan` = no command).
- **Switching an output off, pressing Esc, loading another profile or closing the adapter stops it.**
  When it is stopped from the app, one last frame is sent with each signal's `stop` value
  (for example 0 A) for the nodes it was driving. Outputs without any `stop` value just stop.
- Outputs never restart by themselves when the adapter is opened again.

## `[[command]]` — buttons with replies

```toml
[[command]]
name = "Set velocity interval"
id = "0x020 + n"
data = "22 10 {value:f32be}"
args = [{ name = "value", default = 10, min = 1, max = 100 }]
reply_id = "0x100 + n"
reply = "22"
nack = "FE 22"
timeout_ms = 500
help = "Velocity telemetry period in ms."
```

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Button text |
| `id`, `ext` | required, `false` | Where to send. May use `n` = the node chosen on the page |
| `data` | required | Hex bytes and placeholders, separated by spaces (see below) |
| `args` | `[]` | `{ name, default, min, max }` — an input box per argument |
| `reply_id` | none | ID the answer comes on (needed with `reply`) |
| `reply` | none | Hex prefix of a good answer, e.g. `"22"`. The decoded answer is shown under Results |
| `nack` | none | Hex prefix of a refusal, e.g. `"FE 22"`. Shown in red |
| `timeout_ms` | `500` | After this long without an answer: "no reply" |
| `confirm` | `false` | Ask before sending (for Save, Reboot, …) |
| `help` | none | Tooltip, also shown in the confirmation |

**Placeholders in `data`:** `{arg:type}` inserts argument `arg` encoded as `type` (any
[signal type](#signals)); `{n}` or `{n:type}` inserts the node number. Without a type, `u8` is used.

## Recipes

**A J1939-style 29-bit message**

```toml
[[message]]
name = "engine"
id = 0x0CF00400
ext = true
signals = [{ name = "rpm", byte = 3, type = "u16le", scale = 0.125, unit = "rpm" }]
```

**Bit fields packed into one byte**

```toml
signals = [
  { name = "mode",  byte = 0, type = "u8", bit = 0, bits = 3, enum = { 0 = "OFF", 1 = "POS", 2 = "VEL" } },
  { name = "armed", byte = 0, type = "u8", bit = 7, bits = 1 },
]
```

**One message per node, 8 nodes from 0x200**

```toml
[nodes]
range = [0, 7]
heartbeat = "status"

[[message]]
name = "status"
id = "0x200 + n"
signals = [{ name = "temp", byte = 0, type = "i8", unit = "°C", alarm_above = 80 }]
```

**A setpoint for every node in one frame (4 × i16)**

```toml
[[message]]
name = "setpoints"
id = 0x300
signals = [
  { name = "sp1", byte = 0, type = "i16le", min = -1000, max = 1000, stop = 0 },
  { name = "sp2", byte = 2, type = "i16le", min = -1000, max = 1000, stop = 0 },
  { name = "sp3", byte = 4, type = "i16le", min = -1000, max = 1000, stop = 0 },
  { name = "sp4", byte = 6, type = "i16le", min = -1000, max = 1000, stop = 0 },
]

[[output]]
name = "Setpoints"
message = "setpoints"
period_ms = 20
```
