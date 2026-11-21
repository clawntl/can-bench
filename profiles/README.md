# Device profiles

A device profile is a [TOML](https://toml.io) file that describes **your** CAN devices: which IDs
carry which values, how your boards are numbered, which commands they understand, which values you
drive with sliders, test **sequences** (small scripts), and the **cards** that make up the Device
page. CAN Bench has no project-specific code: everything on that page comes from the profile.

| File | What it is |
|---|---|
| [`bldc-driver.toml`](bldc-driver.toml) | Complete profile for [BLDC-Driver](https://github.com/clawntl/BLDC-Driver) motor drivers. It rebuilds that project's `cantest_gui.py` — motor cards, Auto test, Bus-off test, Settings window, plots — from the generic parts below. A good template. |

**Try it without hardware:** start CAN Bench with `--demo`, pick *Demo bus* on the CAN Terminal
page, click **Open**, then load `bldc-driver.toml` on the Device page. Two simulated boards answer.

**Editing workflow:** keep the file open in your editor and click **⟳ Reload** on the Device page
after each save. Mistakes are reported at the top of the page, naming the message, card or field,
e.g. `card "Left motor": unknown signal "status.sate"`. Unknown keys are errors too, so typos do not
go unnoticed. Sequence scripts are syntax-checked on load.

---

## Contents

**Protocol** · [Minimal profile](#a-minimal-profile) · [Expressions and `n`](#expressions-and-the-node-number-n) ·
[`[bus]`](#bus) · [`[nodes]`](#nodes) · [`[[message]]`](#message) · [Signals](#signals) · [`[[derived]]`](#derived)

**Doing things** · [`[[output]]`](#output--sliders-sent-periodically) · [`[[command]]`](#command--buttons-with-replies) ·
[`[[sequence]]`](#sequence--scripts) · [Script functions](#script-functions)

**The page** · [`[[card]]`](#card) · [Widgets](#widgets) · [Text placeholders](#text-placeholders) · [`[layout]`](#layout)

[Recipes](#recipes)

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

Every frame with ID `0x351` now shows `battery: voltage=52.31 V current=-3.2 A soc=87 %` in the trace.
Without `[[card]]`s the Device page uses the built-in cards (signals, plot, log, …).

## Expressions and the node number `n`

Where a number is expected for an **ID** or a **byte position**, you can also write an expression
in quotes: whole numbers (decimal or `0x` hex), `n` (the node number), `+ - * / %` and parentheses.
`/` is integer division.

```toml
id   = "0x100 + n"               # node 1 -> 0x101, node 2 -> 0x102 …
id   = "0x010 + (n - 1) / 2"     # nodes 1 and 2 share 0x010, nodes 3 and 4 share 0x011 …
byte = "((n - 1) % 2) * 4"       # odd nodes use bytes 0-3, even nodes bytes 4-7
```

A message whose `id` uses `n` exists once per node; a received frame tells CAN Bench which node sent it.

## `[bus]`

The bus settings your devices use. The Device page shows them and offers **Apply** (copies them to
the CAN Terminal page while the adapter is closed).

```toml
[bus]
bitrate = 1000000
sample_point = 87.5          # default 87.5
data_bitrate = 2000000       # CAN FD only
data_sample_point = 75.0     # default 75
```

## `[nodes]`

Only needed if your IDs depend on a node number.

| Key | Default | Meaning |
|---|---|---|
| `range` | required | First and last node number, e.g. `[1, 15]` |
| `label` | `"Node"` | Name used in the UI |
| `heartbeat` | any message | Which received message (or `message.variant`) marks a node as alive |
| `timeout_ms` | `1000` | After this long without a heartbeat the node is *missing* (red, logged) |
| `columns` | none | Signal keys shown as columns in the built-in Nodes card |

## `[[message]]`

```toml
[[message]]
name = "telemetry"
id = "0x100 + n"
mux = { byte = 0 }   # byte 0 says which variant this frame is

  [[message.variant]]
  mux = 0x03
  name = "status"
  signals = [ … ]

  [[message.variant]]          # a second selector byte: GET_CONFIG reply for "pending" settings
  mux = 0x21
  when = { byte = 1, value = 1 }
  name = "config_pending"
  signals = [ … ]
```

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Used in the trace and in signal keys |
| `id` | required | Number or [expression](#expressions-and-the-node-number-n) |
| `ext` | `false` | 29-bit identifier |
| `signals` | `[]` | The [signals](#signals) of a message without variants |
| `mux = { byte = N }` | none | Byte N selects a variant (a "command byte" or "multiplexer") |
| `[[message.variant]]` | none | `mux` = selector value, `name`, `signals`, optional `when = { byte, value }` = a second byte that must match too |
| `length` | `8` | Frame length when CAN Bench **sends** this message (outputs) |
| `fill` | `0` | Value of bytes no signal writes when sending |

Messages are decoded in both directions: frames you send are decoded too, so you can show and plot
what you commanded next to what the board measured.

## Signals

```toml
{ name = "supply", byte = 3, type = "u16be", scale = 0.001, unit = "V", invalid = 0, alarm_below = 10.0 }
```

The **key** of a signal is `variant.signal` (or `message.signal` without variants), e.g.
`status.supply`. Cards, columns and scripts refer to signals by key. **Value = raw × `scale` + `offset`.**

| Key | Default | Meaning |
|---|---|---|
| `name`, `byte`, `type` | required | `byte` may be an expression using `n` |
| `len` | — | Length in bytes, for `type = "ascii"` only |
| `bit`, `bits` | none | Take only `bits` bits starting at bit `bit` (0 = least significant) of an integer |
| `scale`, `offset` | `1`, `0` | |
| `unit` | none | Shown after the value |
| `decimals` | auto | Digits after the point (auto: 3 for floats and fractional scales, else 0) |
| `enum` | none | `{ 1 = "IDLE", 2 = "RUNNING" }`: names instead of numbers |
| `flags` | none | `{ 0 = "TIMEOUT", 3 = "FAULT" }`: bit → name, shown as `TIMEOUT\|FAULT` |
| `invalid` | none | Raw value meaning "no reading" (shown as –, not plotted). Matches the signed value or the bit pattern, so `0x8000` works for an `i16` |
| `alarm` | `[]` | Enum or flag names that are alarms (red) |
| `alarm_below`, `alarm_above` | none | Numeric alarm limits |
| `log_changes` | `false` | Write a line to the log whenever the value changes (e.g. `node 1: status.state IDLE → RUNNING`) |
| `min`, `max` | ±100 | Slider range when the message is an [output](#output--sliders-sent-periodically) |
| `default` | none | Value sent for nodes an output does **not** drive (e.g. `nan` = "no command") |
| `stop` | none | Value sent once when a node stops being driven, or on Esc / STOP |

**Types** — multi-byte types need `be` (big-endian, Motorola) or `le` (little-endian, Intel):
`u8` `i8` · `u16be/le` `i16be/le` · `u24be/le` · `u32be/le` `i32be/le` · `u64be/le` `i64be/le` ·
`f32be/le` `f64be/le` (IEEE floats) · `ascii` (text of `len` bytes, ends at the first 0 byte).

A signal that does not fit in a shorter received frame is skipped, not shown as 0. A NaN float is
shown as `NaN` and not plotted.

## `[[derived]]`

A value computed from the other signals of the same message (a [Rhai](https://rhai.rs) expression;
the variables are the signals' short names). It behaves like any other signal: key `on.name`.

```toml
[[derived]]
name = "power"
on = "status"                       # variant or message
expr = "supply * input_current"
unit = "W"
decimals = 2                        # also: alarm_below, alarm_above
```

## `[[output]]` — sliders sent periodically

```toml
[[output]]
name = "Motor current"
message = "current_cmd"     # a message without variants
period_ms = 10              # default 100
```

Sliders for an output appear in the built-in Outputs card or wherever a card has a
[`slider` widget](#widgets). Each node has a **Send** tick box: while ticked, the frame is sent every
`period_ms` with that node's slider value.

- Timing runs in the adapter thread, like the periodic list, so it keeps going while the window is minimised.
- Several nodes sharing one frame (`id = "0x010 + (n - 1) / 2"`) get one frame per ID. Nodes you do not
  drive get each signal's `default` (BLDC-Driver: `nan` = no command).
- **Unticking Send, Esc, STOP, loading another profile or closing the adapter stops it**; when stopped
  from the app one last frame carries each signal's `stop` value (e.g. 0 A). Nothing restarts by itself.
- If another node on the bus sends the same ID while you do, the Bus widget and the log warn you.

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
| `id`, `ext` | required, `false` | Where to send. May use `n` |
| `data` | required | Hex bytes and placeholders: `{arg:type}` inserts an argument as any signal type, `{n}` / `{n:type}` the node number (default type `u8`) |
| `args` | `[]` | `{ name, default, min, max }` — an input box per argument |
| `reply_id`, `reply` | none | ID and hex prefix of a good answer. The decoded answer is logged |
| `nack` | none | Hex prefix of a refusal, logged in red |
| `timeout_ms` | `500` | After this long without an answer: "no reply" |
| `confirm` | `false` | Ask before sending |
| `help` | none | Tooltip |

## `[[sequence]]` — scripts

A sequence is a script in [Rhai](https://rhai.rs/book/) (a small language that reads like
JavaScript/Rust) that runs in its own thread: send commands and wait for replies, drive outputs,
measure what arrives, ask the user, and report `PASS`/`FAIL` checks. Start one from a
[`button`](#widgets), the built-in Sequences card, or the command line
(`--sequence "Auto test" --node 2 --exit` prints the results and exits with 0 = all passed).

```toml
[[sequence]]
name = "Spin check"
args = [{ name = "amps", default = 0.3, min = 0.0, max = 2.0 }]   # input boxes, variables in the script
stop_after = true          # stop all outputs (with stop values) when it ends
help = "Spins the motor both ways."
script = '''
let r = send("Start FOC", n);
if is_none(r) || r.ok != 1 { abort("FOC did not start"); }
for s in [1, -1] {
    drive("Motor current", n, s * amps);
    wait(2000);
    let m = measure(1000);
    let v = m.avg("velocity.velocity", n);
    check(`direction ${s}`, s * v > 2.0, `mean ${fmt(v, 2)} rad/s`);
}
'''
```

**Esc**, **STOP** and **Abort** stop a running sequence; an aborted sequence always stops all outputs.

### Script functions

Variables: `n` (the node of the card the sequence was started from), `cards` (array of every card's
node), `NAN`, and one variable per `args` entry. Numbers in replies and values are integers when whole.

| Function | Does |
|---|---|
| `send(cmd)` · `send(cmd, node)` · `send(cmd, node, [args])` | Sends a `[[command]]`; waits for its reply. Returns a map of the reply's signals (`r.status`, `r.fw_major`, …; `"nack" in r` on a refusal) or `()` on timeout |
| `send_only(cmd, node [, args])` | Sends without waiting |
| `send_raw(id, "01 02 …")` | Sends any frame |
| `repeat_raw(id, "hex", period_ms, duration_ms)` | Sends a frame repeatedly (blocks) |
| `wait(ms)` | Pause |
| `wait_until(\|\| condition, timeout_ms)` | Waits until the closure returns true; returns false on timeout |
| `value(key [, node])` | Latest value: number, or the name for enums/flags/text; `()` if never seen |
| `text(key [, node])` | Latest value as shown (`"23.91 V"`, `"CMD_TIMEOUT\|INVERTED"`) |
| `age(key, node)` | Milliseconds since the value last arrived |
| `heard(node, ms)` · `nodes_heard(ms)` | Heartbeat within `ms` |
| `measure(ms)` | Collects for `ms` and returns a measurement `m` (below) |
| `start_measure()` … `.stop()` | The same around a longer part of the script |
| `m.count(name, node)` | Frames of a message/variant (`"status"`), or updates of a signal (`"status.state"`) |
| `m.avg / min / max / last(key, node)` | Statistics of a numeric signal (`()` if none) |
| `m.seen(key, node, "NAME")` | Whether a value/flag name appeared in any frame |
| `m.bus_errors()` · `bus_errors()` | Bus error reports (in the window / since start) |
| `drive(output, node, value)` | Sets the slider and ticks Send (value or array for multi-signal outputs) |
| `release(output, node)` | Unticks Send for that node (sends its stop value once) |
| `stop()` | Stops every output |
| `slider(output, node)` | Current slider value |
| `check(name, ok [, detail])` | Logs `PASS`/`FAIL`, counts it, returns `ok` |
| `log` · `info` · `warn` · `error` · `prompt(text)` | Log lines (prompt = highlighted instruction) |
| `confirm(text)` | Asks the user OK/Cancel; returns true/false |
| `abort(text)` | Ends the sequence as failed |
| `set_node(n)` | Changes the node of the card the sequence was started from (follow a renamed board) |
| `fmt(x, decimals)` · `abs` · `min` · `max` · `is_none(x)` · `is_nan(x)` | Helpers |
| `timestamp()` / `t.elapsed` | Timing (Rhai built-in; seconds) |

Rhai's own features work too: `let`, `if`/`else` (also as expressions), `for x in [..]`, `for i in 1..6`,
`while`, closures, string interpolation `` `node ${n}` ``, `"x" in text`, arrays (`.push`, `.all(|x| …)`), maps.

## `[[card]]`

Cards are the boxes on the Device page. Each card has a list of [widgets](#widgets).

```toml
[[card]]
node_select = true                       # node spinner in the card's title
instances = [                            # the same card several times, one per node
  { title = "Left motor",  node = 1, color = "#268bd2" },
  { title = "Right motor", node = 2, color = "#d33682" },
]
widgets = [ … ]

[[card]]
title = "Settings"
popup = true                             # a window, opened by a button with open = "Settings"
widgets = [ … ]
```

| Key | Default | Meaning |
|---|---|---|
| `title` | required without `instances` | Unique name; also used by `[layout]` and `open =` |
| `instances` | none | `{ title, node, color }` per copy of the card |
| `node` | first node | Node `n` the card's widgets use |
| `node_select` | `false` | Node spinner in the title bar |
| `color` | none | Accent colour (`"#rrggbb"`, `"#rgb"` or green/red/orange/yellow/blue/purple/gray); also the card's line colour in plots |
| `popup` | `false` | Not on the page; a window opened by a `button` with `open`, for the opener card's node |

The built-in cards can be used in `[layout]` by these names: `@nodes`, `@signals`, `@plot` (ticked
signals), `@outputs`, `@commands`, `@sequences`, `@log`, `@bus`.

## Widgets

Every widget is a table with a `type`. In a card, `n` is the card's node.

| `type` | Keys | Shows |
|---|---|---|
| `text` | `text`, `large`, `small`, `color`, `stale_ms`, `stale_text` | Text with [placeholders](#text-placeholders). With `stale_ms`, shows `stale_text` when a value is missing or older |
| `value` | `signal`, `label`, `large`, `stale_ms` | One value (red when in alarm) |
| `badge` | `signal`, `label`, `colors = { RUNNING = "green", … }`, `stale_ms` | A value as a coloured tag |
| `flags` | `signal`, `stale_ms` | Every flag of a flags signal as a chip: set = blue, set alarm = red |
| `online` | `online`, `offline` | Whether the card's node sent a heartbeat within `[nodes].timeout_ms` |
| `ids` | `messages = [..]` | The CAN IDs of those messages for the card's node |
| `slider` | `output`, `signal`, `label`, `presets = [..]`, `send_toggle` (default true) | Send tick box, slider and preset buttons for an output, for the card's node |
| `link` | `output`, `label` | Tick box: moving one card's slider (or Send) moves all of them |
| `button` | one of `command`, `sequence`, `open`, `action`; `label`, `confirm`, `inputs` (default true), `danger` | Runs a command (its argument boxes shown next to it), a sequence (its `args` boxes), opens a popup card, or `action = "stop"` (Esc), `"abort"`, `"stop_output"` (+ `output`). `confirm` asks first (placeholders allowed); `danger` = red |
| `plot` | `series = [{ signal, node, color, label }]`, `title`, `seconds` (10), `height` (180), `min_range`, `gap_ms` (300) | A scrolling plot. `node` = a number, `"card"` (default), `"cards"` (every card's node in its colour) or `"all"`. The line breaks where values stop for longer than `gap_ms` |
| `rate` | `message`, `label`, `gap_warn_ms` | Frames/s **received** for that message/variant, longest gap, gaps over the limit (spot another controller and its timing) |
| `log` | `height` | The event log |
| `bus` | | Rx/Tx counts, bus errors, nodes heard, the running sequence, "another node also sends …" warnings |
| `row` | `widgets = [..]` | Widgets side by side (wrapping) |
| `separator` | | A line |
| `nodes` `signals` `outputs` `commands` `sequences` `plot_ticked` | | The built-in cards' contents inside your own card |

## Text placeholders

In `text` widgets and button `confirm` texts:

| Placeholder | Becomes |
|---|---|
| `{status.supply}` | The value as shown (with unit), for the card's node |
| `{raw:status.supply}` | The number only |
| `{hex:status.flags}` | The number in hex |
| `{count:error}` | How many frames of that message/variant the card's node sent |
| `{n}` · `{rx}` · `{tx}` · `{bus_errors}` | Node number, frame counters, bus error reports |

Unknown placeholders are reported when the profile loads.

## `[layout]`

```toml
[layout]
columns = [["Left motor", "Both", "Log"], ["Right motor", "Velocity and current"]]
widths = [1.0, 1.0]          # relative column widths
```

You rarely write this by hand: on the Device page click **✏ Edit layout**, then

- drag cards by **⠿** to another place or column,
- drag the bars between columns to resize them, **➕ Column** / **✖ remove column**,
- **✖** on a card hides it; hidden cards and the built-in cards are listed under **Add**,
- **💾 Save to profile** writes `[layout]` into the file (everything else in the file, comments
  included, is kept), **Done** keeps it for this session only, **Reset** goes back to the file's layout.

Without `[layout]`, cards are placed in two columns in file order. Popup cards are never in the layout.

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

**One status card per node, eight nodes from 0x200**

```toml
[nodes]
range = [0, 7]
heartbeat = "status"

[[message]]
name = "status"
id = "0x200 + n"
signals = [{ name = "temp", byte = 0, type = "i8", unit = "°C", alarm_above = 80, log_changes = true }]

[[card]]
title = "Sensor"
node_select = true
widgets = [
  { type = "row", widgets = [{ type = "online" }, { type = "value", signal = "status.temp", large = true, stale_ms = 1000 }] },
  { type = "plot", series = [{ signal = "status.temp" }], seconds = 60 },
]
```

**A setpoint per axis in one frame, with presets and a STOP button**

```toml
[[message]]
name = "setpoints"
id = 0x300
signals = [
  { name = "x", byte = 0, type = "i16le", min = -1000, max = 1000, stop = 0 },
  { name = "y", byte = 2, type = "i16le", min = -1000, max = 1000, stop = 0 },
]

[[output]]
name = "Setpoints"
message = "setpoints"
period_ms = 20

[[card]]
title = "Gantry"
widgets = [
  { type = "slider", output = "Setpoints", signal = "x", presets = [-500, 0, 500] },
  { type = "slider", output = "Setpoints", signal = "y", send_toggle = false },
  { type = "button", action = "stop", label = "STOP (Esc)", danger = true },
]
```
