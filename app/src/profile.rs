//! Device profiles: a TOML file that describes one project's CAN protocol, so the window can
//! decode frames into named signals, track nodes, send commands and drive signal outputs
//! without any project-specific code. See `profiles/README.md` for the format.

use crate::can::CanFrame;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

// ============================================================ integer expressions ("0x100 + n")

/// A tiny integer expression: numbers (decimal or 0x hex), the node number `n`,
/// + - * / % and parentheses. Division is integer division.
#[derive(Clone, Debug)]
pub enum Expr {
    Num(i64),
    N,
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn parse(text: &str) -> Result<Expr, String> {
        let toks = tokenize(text)?;
        let mut p = 0;
        let e = parse_sum(&toks, &mut p)?;
        if p != toks.len() {
            return Err(format!("unexpected '{}' in \"{text}\"", toks[p]));
        }
        Ok(e)
    }

    pub fn uses_n(&self) -> bool {
        match self {
            Expr::Num(_) => false,
            Expr::N => true,
            Expr::Neg(a) => a.uses_n(),
            Expr::Bin(_, a, b) => a.uses_n() || b.uses_n(),
        }
    }

    pub fn eval(&self, n: i64) -> Result<i64, String> {
        Ok(match self {
            Expr::Num(v) => *v,
            Expr::N => n,
            Expr::Neg(a) => -a.eval(n)?,
            Expr::Bin(op, a, b) => {
                let (a, b) = (a.eval(n)?, b.eval(n)?);
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' | '%' if b == 0 => return Err("division by zero".into()),
                    '/' => a.div_euclid(b),
                    '%' => a.rem_euclid(b),
                    _ => unreachable!(),
                }
            }
        })
    }
}

fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if "+-*/%()".contains(c) {
            out.push(c.to_string());
            chars.next();
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    s.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(s);
        } else {
            return Err(format!("unexpected character '{c}' in \"{text}\""));
        }
    }
    Ok(out)
}

fn parse_sum(t: &[String], p: &mut usize) -> Result<Expr, String> {
    let mut e = parse_product(t, p)?;
    while *p < t.len() && (t[*p] == "+" || t[*p] == "-") {
        let op = t[*p].chars().next().unwrap();
        *p += 1;
        e = Expr::Bin(op, Box::new(e), Box::new(parse_product(t, p)?));
    }
    Ok(e)
}

fn parse_product(t: &[String], p: &mut usize) -> Result<Expr, String> {
    let mut e = parse_atom(t, p)?;
    while *p < t.len() && (t[*p] == "*" || t[*p] == "/" || t[*p] == "%") {
        let op = t[*p].chars().next().unwrap();
        *p += 1;
        e = Expr::Bin(op, Box::new(e), Box::new(parse_atom(t, p)?));
    }
    Ok(e)
}

fn parse_atom(t: &[String], p: &mut usize) -> Result<Expr, String> {
    let tok = t.get(*p).ok_or("expression ends too early")?;
    *p += 1;
    match tok.as_str() {
        "(" => {
            let e = parse_sum(t, p)?;
            if t.get(*p).map(String::as_str) != Some(")") {
                return Err("missing ')'".into());
            }
            *p += 1;
            Ok(e)
        }
        "-" => Ok(Expr::Neg(Box::new(parse_atom(t, p)?))),
        "n" => Ok(Expr::N),
        s => parse_int(s).map(Expr::Num).ok_or_else(|| format!("unknown name '{s}' (only n is known)")),
    }
}

pub fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(&h.replace('_', ""), 16).ok()
    } else {
        s.replace('_', "").parse().ok()
    }
}

// ============================================================ value types

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ty {
    U(usize),
    I(usize),
    F32,
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kind {
    pub ty: Ty,
    pub big_endian: bool,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind, String> {
        let s = s.trim().to_ascii_lowercase();
        let (base, be) = if let Some(b) = s.strip_suffix("be") {
            (b.to_string(), true)
        } else if let Some(b) = s.strip_suffix("le") {
            (b.to_string(), false)
        } else {
            (s.clone(), true)
        };
        let ty = match base.as_str() {
            "u8" => Ty::U(1),
            "i8" => Ty::I(1),
            "u16" => Ty::U(2),
            "i16" => Ty::I(2),
            "u24" => Ty::U(3),
            "u32" => Ty::U(4),
            "i32" => Ty::I(4),
            "u64" => Ty::U(8),
            "i64" => Ty::I(8),
            "f32" => Ty::F32,
            "f64" => Ty::F64,
            _ => return Err(format!("unknown type '{s}' (u8, i8, u16be, i16le, u32be, i32be, f32be, f64le, …)")),
        };
        if matches!(base.as_str(), "u8" | "i8") && s != base {
            return Err(format!("'{s}': one-byte types have no byte order"));
        }
        if !matches!(base.as_str(), "u8" | "i8") && s == base {
            return Err(format!("'{s}': say the byte order, e.g. {s}be or {s}le"));
        }
        Ok(Kind { ty, big_endian: be })
    }

    pub fn size(&self) -> usize {
        match self.ty {
            Ty::U(n) | Ty::I(n) => n,
            Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }

    fn is_float(&self) -> bool {
        matches!(self.ty, Ty::F32 | Ty::F64)
    }

    /// Unsigned bit pattern of the field.
    fn read_bits(&self, bytes: &[u8]) -> u64 {
        let mut v = 0u64;
        if self.big_endian {
            for b in bytes {
                v = (v << 8) | *b as u64;
            }
        } else {
            for b in bytes.iter().rev() {
                v = (v << 8) | *b as u64;
            }
        }
        v
    }

    fn write_bits(&self, v: u64, out: &mut [u8]) {
        let n = out.len();
        for i in 0..n {
            let byte = (v >> (8 * i)) as u8;
            if self.big_endian {
                out[n - 1 - i] = byte;
            } else {
                out[i] = byte;
            }
        }
    }
}

// ============================================================ TOML schema

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
enum IntOrStr {
    Int(i64),
    Str(String),
}

impl IntOrStr {
    fn expr(&self) -> Result<Expr, String> {
        match self {
            IntOrStr::Int(v) => Ok(Expr::Num(*v)),
            IntOrStr::Str(s) => Expr::parse(s),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDef {
    name: Option<String>,
    nodes: Option<NodesDef>,
    #[serde(default, rename = "message")]
    messages: Vec<MessageDef>,
    #[serde(default, rename = "command")]
    commands: Vec<CommandDef>,
    #[serde(default, rename = "output")]
    outputs: Vec<OutputDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodesDef {
    range: [i64; 2],
    label: Option<String>,
    heartbeat: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default)]
    columns: Vec<String>,
}

fn default_timeout() -> u64 {
    1000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageDef {
    name: String,
    id: IntOrStr,
    #[serde(default)]
    ext: bool,
    mux: Option<MuxDef>,
    #[serde(default)]
    signals: Vec<SignalDef>,
    #[serde(default, rename = "variant")]
    variants: Vec<VariantDef>,
    length: Option<usize>,
    #[serde(default)]
    fill: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxDef {
    byte: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantDef {
    mux: i64,
    name: String,
    #[serde(default)]
    signals: Vec<SignalDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalDef {
    name: String,
    byte: IntOrStr,
    #[serde(rename = "type")]
    ty: String,
    bit: Option<u8>,
    bits: Option<u8>,
    #[serde(default = "one")]
    scale: f64,
    #[serde(default)]
    offset: f64,
    unit: Option<String>,
    #[serde(rename = "enum")]
    enums: Option<BTreeMap<String, String>>,
    flags: Option<BTreeMap<String, String>>,
    invalid: Option<i64>,
    #[serde(default)]
    alarm: Vec<String>,
    alarm_below: Option<f64>,
    alarm_above: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    default: Option<f64>,
    stop: Option<f64>,
    decimals: Option<usize>,
}

fn one() -> f64 {
    1.0
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandDef {
    name: String,
    id: IntOrStr,
    #[serde(default)]
    ext: bool,
    data: String,
    #[serde(default)]
    args: Vec<ArgDef>,
    reply: Option<String>,
    nack: Option<String>,
    reply_id: Option<IntOrStr>,
    #[serde(default = "default_reply_timeout")]
    timeout_ms: u64,
    #[serde(default)]
    confirm: bool,
    help: Option<String>,
}

fn default_reply_timeout() -> u64 {
    500
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArgDef {
    name: String,
    #[serde(default)]
    default: f64,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputDef {
    name: String,
    message: String,
    #[serde(default = "default_period")]
    period_ms: u64,
}

fn default_period() -> u64 {
    100
}

// ============================================================ compiled profile

pub struct Nodes {
    pub min: i64,
    pub max: i64,
    pub label: String,
    /// message index (and variant) whose arrival marks a node alive; None = any message
    pub heartbeat: Option<(usize, Option<usize>)>,
    pub timeout_ms: u64,
    pub columns: Vec<String>,
}

pub struct Signal {
    pub name: String,
    byte: Expr,
    kind: Kind,
    bitfield: Option<(u8, u8)>,
    pub scale: f64,
    pub offset: f64,
    pub unit: String,
    enums: BTreeMap<i64, String>,
    flags: BTreeMap<u8, String>,
    invalid: Option<i64>,
    alarm: Vec<String>,
    alarm_below: Option<f64>,
    alarm_above: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub default: Option<f64>,
    pub stop: Option<f64>,
    decimals: Option<usize>,
}

pub struct Variant {
    pub mux: i64,
    pub name: String,
    pub signals: Vec<Signal>,
}

pub struct Message {
    pub name: String,
    id: Expr,
    pub ext: bool,
    pub per_node: bool,
    mux_byte: Option<usize>,
    pub signals: Vec<Signal>,
    pub variants: Vec<Variant>,
    length: usize,
    fill: u8,
}

pub enum Tok {
    Byte(u8),
    Arg(String, Kind),
}

pub struct Command {
    pub name: String,
    id: Expr,
    ext: bool,
    data: Vec<Tok>,
    pub args: Vec<(String, f64, Option<f64>, Option<f64>)>,
    reply_id: Option<Expr>,
    reply: Option<Vec<u8>>,
    nack: Option<Vec<u8>>,
    pub timeout_ms: u64,
    pub confirm: bool,
    pub help: String,
}

pub struct Output {
    pub name: String,
    pub message: usize,
    pub period_ms: u64,
}

pub struct Profile {
    pub name: String,
    pub nodes: Option<Nodes>,
    pub messages: Vec<Message>,
    pub commands: Vec<Command>,
    pub outputs: Vec<Output>,
    /// (ext, id) -> [(message, node)]
    index: HashMap<(bool, u32), Vec<(usize, Option<i64>)>>,
}

/// One decoded signal value.
#[derive(Clone, Debug)]
pub struct Value {
    /// "variant.signal", or "message.signal" for messages without variants
    pub key: String,
    /// scaled numeric value; None when the field says "no value" (NaN, `invalid`)
    pub num: Option<f64>,
    pub text: String,
    pub alarm: bool,
}

/// One message decoded from one frame (a frame shared by several nodes gives one per node).
#[derive(Clone, Debug)]
pub struct Decoded {
    pub message: usize,
    pub variant: Option<usize>,
    pub node: Option<i64>,
    pub values: Vec<Value>,
}

impl Profile {
    pub fn load(path: &Path) -> Result<Profile, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        Profile::parse(&text, &stem)
    }

    pub fn parse(text: &str, fallback_name: &str) -> Result<Profile, String> {
        let def: FileDef = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut messages = Vec::new();
        for m in &def.messages {
            messages.push(compile_message(m).map_err(|e| format!("message \"{}\": {e}", m.name))?);
        }
        let find_msg = |name: &str| -> Result<(usize, Option<usize>), String> {
            let (m, v) = name.split_once('.').map(|(a, b)| (a, Some(b))).unwrap_or((name, None));
            let mi = messages.iter().position(|x| x.name == m).ok_or_else(|| format!("no message called \"{m}\""))?;
            let vi = match v {
                Some(v) => Some(messages[mi].variants.iter().position(|x| x.name == v).ok_or_else(|| format!("message \"{m}\" has no variant \"{v}\""))?),
                None => None,
            };
            Ok((mi, vi))
        };
        let nodes = match &def.nodes {
            Some(n) => {
                if n.range[0] > n.range[1] {
                    return Err("nodes.range: first number must not be bigger than the second".into());
                }
                Some(Nodes {
                    min: n.range[0],
                    max: n.range[1],
                    label: n.label.clone().unwrap_or_else(|| "Node".into()),
                    heartbeat: match &n.heartbeat {
                        Some(h) => Some(find_msg(h).map_err(|e| format!("nodes.heartbeat: {e}"))?),
                        None => None,
                    },
                    timeout_ms: n.timeout_ms,
                    columns: n.columns.clone(),
                })
            }
            None => None,
        };
        let node_range: Vec<i64> = nodes.as_ref().map(|n| (n.min..=n.max).collect()).unwrap_or_default();

        let mut index: HashMap<(bool, u32), Vec<(usize, Option<i64>)>> = HashMap::new();
        for (mi, m) in messages.iter().enumerate() {
            if m.per_node {
                if nodes.is_none() {
                    return Err(format!("message \"{}\" uses n, so the file needs a [nodes] section", m.name));
                }
                for &n in &node_range {
                    let id = m.id_for(n).map_err(|e| format!("message \"{}\": {e}", m.name))?;
                    index.entry((m.ext, id)).or_default().push((mi, Some(n)));
                }
            } else {
                let id = m.id_for(0).map_err(|e| format!("message \"{}\": {e}", m.name))?;
                index.entry((m.ext, id)).or_default().push((mi, None));
            }
        }

        let mut commands = Vec::new();
        for c in &def.commands {
            commands.push(compile_command(c, nodes.is_some()).map_err(|e| format!("command \"{}\": {e}", c.name))?);
        }
        let mut outputs = Vec::new();
        for o in &def.outputs {
            let (mi, vi) = find_msg(&o.message).map_err(|e| format!("output \"{}\": {e}", o.name))?;
            if vi.is_some() || !messages[mi].variants.is_empty() {
                return Err(format!("output \"{}\": the message must be a plain one (no mux/variants)", o.name));
            }
            if messages[mi].signals.is_empty() {
                return Err(format!("output \"{}\": message \"{}\" has no signals", o.name, o.message));
            }
            outputs.push(Output { name: o.name.clone(), message: mi, period_ms: o.period_ms.clamp(1, 60_000) });
        }
        Ok(Profile {
            name: def.name.unwrap_or_else(|| fallback_name.to_string()),
            nodes,
            messages,
            commands,
            outputs,
            index,
        })
    }

    pub fn node_range(&self) -> Vec<i64> {
        self.nodes.as_ref().map(|n| (n.min..=n.max).collect()).unwrap_or_default()
    }

    pub fn decode(&self, f: &CanFrame) -> Vec<Decoded> {
        let Some(list) = self.index.get(&(f.ext, f.id)) else { return Vec::new() };
        if f.rtr {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &(mi, node) in list {
            let m = &self.messages[mi];
            let n = node.unwrap_or(0);
            let (variant, signals, prefix) = match m.mux_byte {
                Some(b) => {
                    let Some(&sel) = f.data.get(b) else { continue };
                    match m.variants.iter().position(|v| v.mux == sel as i64) {
                        Some(vi) => (Some(vi), &m.variants[vi].signals, &m.variants[vi].name),
                        None => (None, &m.signals, &m.name),
                    }
                }
                None => (None, &m.signals, &m.name),
            };
            let values = signals.iter().filter_map(|s| s.decode(&f.data, n).map(|(num, text, alarm)| Value { key: format!("{prefix}.{}", s.name), num, text, alarm })).collect();
            out.push(Decoded { message: mi, variant, node, values });
        }
        out
    }

    /// Short text for the trace: "status n1: state=RUNNING supply=24.01 V".
    pub fn describe(&self, d: &Decoded) -> String {
        let m = &self.messages[d.message];
        let mut s = match d.variant {
            Some(v) => m.variants[v].name.clone(),
            None if m.mux_byte.is_some() => format!("{} (unknown)", m.name),
            None => m.name.clone(),
        };
        if let Some(n) = d.node {
            s += &format!(" n{n}");
        }
        if !d.values.is_empty() {
            s.push(':');
            for v in &d.values {
                let short = v.key.rsplit('.').next().unwrap_or(&v.key);
                s += &format!(" {short}={}", v.text);
            }
        }
        s
    }

    /// Frames for an output: `values(n)` gives the signal values for a node that this output
    /// drives (None = not driven). Nodes that share a frame but are not driven get each signal's
    /// `default`. For a message without n, `values(0)` is used.
    pub fn encode_output(&self, out: usize, values: &dyn Fn(i64) -> Option<Vec<f64>>) -> Result<Vec<CanFrame>, String> {
        let m = &self.messages[self.outputs[out].message];
        let nodes = if m.per_node { self.node_range() } else { vec![0] };
        let mut frames: BTreeMap<u32, (Vec<u8>, bool)> = BTreeMap::new();
        for &n in &nodes {
            let id = m.id_for(n)?;
            let vals = values(n);
            let entry = frames.entry(id).or_insert_with(|| (vec![m.fill; m.length], false));
            if vals.is_some() {
                entry.1 = true;
            }
            for (i, s) in m.signals.iter().enumerate() {
                let v = match &vals {
                    Some(v) => v.get(i).copied().or(s.default),
                    None => s.default,
                };
                if let Some(v) = v {
                    s.encode(v, n, &mut entry.0).map_err(|e| format!("{}: {e}", s.name))?;
                }
            }
        }
        Ok(frames
            .into_iter()
            .filter(|(_, (_, driven))| *driven)
            .map(|(id, (data, _))| CanFrame { id, ext: m.ext, rtr: false, fd: data.len() > 8, brs: false, esi: false, data })
            .collect())
    }

    /// Builds a command frame for node `n` with argument values.
    pub fn build_command(&self, ci: usize, n: i64, args: &[f64]) -> Result<CanFrame, String> {
        let c = &self.commands[ci];
        let id = c.id.eval(n)?;
        let id = u32::try_from(id).map_err(|_| "negative CAN ID".to_string())?;
        let mut data = Vec::new();
        for t in &c.data {
            match t {
                Tok::Byte(b) => data.push(*b),
                Tok::Arg(name, kind) => {
                    let v = if name == "n" {
                        n as f64
                    } else {
                        let i = c.args.iter().position(|a| &a.0 == name).ok_or_else(|| format!("unknown argument {name}"))?;
                        args.get(i).copied().unwrap_or(c.args[i].1)
                    };
                    let mut buf = vec![0u8; kind.size()];
                    write_value(*kind, v, &mut buf)?;
                    data.extend(buf);
                }
            }
        }
        if data.len() > 64 {
            return Err("more than 64 data bytes".into());
        }
        Ok(CanFrame { id, ext: c.ext || id > 0x7FF, rtr: false, fd: data.len() > 8, brs: false, esi: false, data })
    }

    /// What a reply to command `ci` sent to node `n` looks like: (ext, id, reply prefix, nack prefix).
    pub fn reply_match(&self, ci: usize, n: i64) -> Option<(bool, u32, Vec<u8>, Option<Vec<u8>>)> {
        let c = &self.commands[ci];
        let prefix = c.reply.clone()?;
        let id = c.reply_id.as_ref()?.eval(n).ok()?;
        Some((c.ext || id > 0x7FF, u32::try_from(id).ok()?, prefix, c.nack.clone()))
    }
}

impl Message {
    pub fn id_for(&self, n: i64) -> Result<u32, String> {
        let id = self.id.eval(n)?;
        let max = if self.ext { 0x1FFF_FFFF } else { 0x7FF };
        if !(0..=max).contains(&id) {
            return Err(format!("ID 0x{id:X} (n = {n}) is out of range"));
        }
        Ok(id as u32)
    }
}

impl Signal {
    pub fn decimals(&self) -> usize {
        self.decimals.unwrap_or(if self.kind.is_float() || self.scale.fract() != 0.0 { 3 } else { 0 })
    }

    /// (number, text, alarm); None when the frame is too short for this signal.
    fn decode(&self, data: &[u8], n: i64) -> Option<(Option<f64>, String, bool)> {
        let at = usize::try_from(self.byte.eval(n).ok()?).ok()?;
        let size = self.kind.size();
        let bytes = data.get(at..at + size)?;
        let mut bits = self.kind.read_bits(bytes);
        if let Some((bit, len)) = self.bitfield {
            bits = (bits >> bit) & if len >= 64 { u64::MAX } else { (1u64 << len) - 1 };
        }
        let raw: f64 = match self.kind.ty {
            Ty::F32 => f32::from_bits(bits as u32) as f64,
            Ty::F64 => f64::from_bits(bits),
            Ty::U(_) => bits as f64,
            Ty::I(sz) => {
                let width = self.bitfield.map(|b| b.1 as u32).unwrap_or(sz as u32 * 8);
                let shift = 64 - width;
                (((bits << shift) as i64) >> shift) as f64
            }
        };
        if let Some(inv) = self.invalid {
            if raw == inv as f64 || bits == inv as u64 {
                return Some((None, "–".into(), false));
            }
        }
        if raw.is_nan() {
            return Some((None, "NaN".into(), false));
        }
        if !self.enums.is_empty() {
            let name = self.enums.get(&(raw as i64)).cloned().unwrap_or_else(|| format!("{}?", raw as i64));
            let alarm = self.alarm.contains(&name);
            return Some((Some(raw), name, alarm));
        }
        if !self.flags.is_empty() {
            let set: Vec<&String> = self.flags.iter().filter(|(b, _)| bits & (1u64 << **b) != 0).map(|(_, n)| n).collect();
            let alarm = set.iter().any(|n| self.alarm.contains(n));
            let text = if set.is_empty() { "–".into() } else { set.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("|") };
            return Some((Some(bits as f64), text, alarm));
        }
        let v = raw * self.scale + self.offset;
        let alarm = self.alarm_below.is_some_and(|b| v < b) || self.alarm_above.is_some_and(|a| v > a);
        let mut text = format!("{v:.*}", self.decimals());
        if !self.unit.is_empty() {
            text = format!("{text} {}", self.unit);
        }
        Some((Some(v), text, alarm))
    }

    fn encode(&self, v: f64, n: i64, data: &mut Vec<u8>) -> Result<(), String> {
        if self.bitfield.is_some() {
            return Err("bit fields cannot be sent".into());
        }
        let at = usize::try_from(self.byte.eval(n)?).map_err(|_| "negative byte position")?;
        let end = at + self.kind.size();
        if end > data.len() {
            data.resize(end, 0);
        }
        let raw = if v.is_nan() { v } else { (v - self.offset) / self.scale };
        write_value(self.kind, raw, &mut data[at..end])
    }
}

fn write_value(kind: Kind, v: f64, out: &mut [u8]) -> Result<(), String> {
    let bits = match kind.ty {
        Ty::F32 => (v as f32).to_bits() as u64,
        Ty::F64 => v.to_bits(),
        Ty::U(n) => {
            if v.is_nan() {
                return Err("NaN cannot be sent as an integer".into());
            }
            let max = if n >= 8 { u64::MAX as f64 } else { ((1u64 << (8 * n)) - 1) as f64 };
            v.round().clamp(0.0, max) as u64
        }
        Ty::I(n) => {
            if v.is_nan() {
                return Err("NaN cannot be sent as an integer".into());
            }
            let half = (1i128 << (8 * n - 1)) as f64;
            (v.round().clamp(-half, half - 1.0) as i64) as u64
        }
    };
    kind.write_bits(bits, out);
    Ok(())
}

fn compile_signal(s: &SignalDef) -> Result<Signal, String> {
    let kind = Kind::parse(&s.ty)?;
    let bitfield = match (s.bit, s.bits) {
        (None, None) => None,
        (bit, bits) => {
            if kind.is_float() {
                return Err(format!("{}: bit fields need an integer type", s.name));
            }
            let (b, l) = (bit.unwrap_or(0), bits.unwrap_or(1));
            if b as usize + l as usize > kind.size() * 8 || l == 0 {
                return Err(format!("{}: bit {b} + {l} bits does not fit in {}", s.name, s.ty));
            }
            Some((b, l))
        }
    };
    let mut enums = BTreeMap::new();
    for (k, v) in s.enums.iter().flatten() {
        enums.insert(parse_int(k).ok_or_else(|| format!("{}: enum key '{k}' is not a number", s.name))?, v.clone());
    }
    let mut flags = BTreeMap::new();
    for (k, v) in s.flags.iter().flatten() {
        let b = parse_int(k).filter(|b| (0..64).contains(b)).ok_or_else(|| format!("{}: flag key '{k}' must be a bit number 0-63", s.name))?;
        flags.insert(b as u8, v.clone());
    }
    for a in &s.alarm {
        if !enums.values().any(|v| v == a) && !flags.values().any(|v| v == a) {
            return Err(format!("{}: alarm \"{a}\" is not one of its enum or flag names", s.name));
        }
    }
    if s.scale == 0.0 {
        return Err(format!("{}: scale must not be 0", s.name));
    }
    Ok(Signal {
        name: s.name.clone(),
        byte: s.byte.expr().map_err(|e| format!("{}: byte: {e}", s.name))?,
        kind,
        bitfield,
        scale: s.scale,
        offset: s.offset,
        unit: s.unit.clone().unwrap_or_default(),
        enums,
        flags,
        invalid: s.invalid,
        alarm: s.alarm.clone(),
        alarm_below: s.alarm_below,
        alarm_above: s.alarm_above,
        min: s.min,
        max: s.max,
        default: s.default,
        stop: s.stop,
        decimals: s.decimals,
    })
}

fn compile_message(m: &MessageDef) -> Result<Message, String> {
    let id = m.id.expr().map_err(|e| format!("id: {e}"))?;
    let signals = m.signals.iter().map(compile_signal).collect::<Result<Vec<_>, _>>()?;
    let mut variants = Vec::new();
    for v in &m.variants {
        variants.push(Variant { mux: v.mux, name: v.name.clone(), signals: v.signals.iter().map(compile_signal).collect::<Result<Vec<_>, _>>().map_err(|e| format!("variant \"{}\": {e}", v.name))? });
    }
    if !variants.is_empty() && m.mux.is_none() {
        return Err("variants need mux = { byte = … }".into());
    }
    Ok(Message {
        name: m.name.clone(),
        per_node: id.uses_n(),
        id,
        ext: m.ext,
        mux_byte: m.mux.as_ref().map(|x| x.byte),
        signals,
        variants,
        length: m.length.unwrap_or(8).clamp(0, 64),
        fill: m.fill,
    })
}

fn parse_hex_prefix(s: &str) -> Result<Vec<u8>, String> {
    s.split_whitespace().map(|t| u8::from_str_radix(t, 16).map_err(|_| format!("'{t}' is not a hex byte"))).collect()
}

fn compile_command(c: &CommandDef, has_nodes: bool) -> Result<Command, String> {
    let id = c.id.expr().map_err(|e| format!("id: {e}"))?;
    let args: Vec<(String, f64, Option<f64>, Option<f64>)> = c.args.iter().map(|a| (a.name.clone(), a.default, a.min, a.max)).collect();
    let mut data = Vec::new();
    for t in c.data.split_whitespace() {
        if let Some(inner) = t.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            let (name, ty) = inner.split_once(':').unwrap_or((inner, "u8"));
            if name != "n" && !args.iter().any(|a| a.0 == name) {
                return Err(format!("data uses {{{name}}} but there is no argument called {name}"));
            }
            data.push(Tok::Arg(name.to_string(), Kind::parse(ty)?));
        } else {
            data.push(Tok::Byte(u8::from_str_radix(t, 16).map_err(|_| format!("data: '{t}' is not a hex byte or {{argument:type}}"))?));
        }
    }
    let per_node = id.uses_n() || data.iter().any(|t| matches!(t, Tok::Arg(n, _) if n == "n"));
    if per_node && !has_nodes {
        return Err("uses n, so the file needs a [nodes] section".into());
    }
    let reply = c.reply.as_deref().map(parse_hex_prefix).transpose().map_err(|e| format!("reply: {e}"))?;
    let nack = c.nack.as_deref().map(parse_hex_prefix).transpose().map_err(|e| format!("nack: {e}"))?;
    if reply.is_some() && c.reply_id.is_none() {
        return Err("reply needs reply_id (the CAN ID the answer comes on)".into());
    }
    Ok(Command {
        name: c.name.clone(),
        id,
        ext: c.ext,
        data,
        args,
        reply_id: c.reply_id.as_ref().map(|r| r.expr()).transpose().map_err(|e| format!("reply_id: {e}"))?,
        reply,
        nack,
        timeout_ms: c.timeout_ms,
        confirm: c.confirm,
        help: c.help.clone().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u32, data: &[u8]) -> CanFrame {
        CanFrame { id, ext: false, rtr: false, fd: false, brs: false, esi: false, data: data.to_vec() }
    }

    #[test]
    fn expressions() {
        let e = Expr::parse("0x010 + (n - 1) / 2").unwrap();
        assert_eq!((e.eval(1).unwrap(), e.eval(2).unwrap(), e.eval(3).unwrap()), (0x10, 0x10, 0x11));
        assert_eq!(Expr::parse("((n-1)%2)*4").unwrap().eval(2).unwrap(), 4);
        assert!(Expr::parse("0x10 + m").is_err());
        assert!(Expr::parse("(1 + 2").is_err());
    }

    #[test]
    fn bldc_example_profile() {
        let text = include_str!("../../profiles/bldc-driver.toml");
        let p = Profile::parse(text, "x").unwrap();

        // heartbeat from node 2: running, inverted, 24.012 V, 1.5 A
        let d = p.decode(&frame(0x102, &[0x03, 2, 0x04, 0x5D, 0xCC, 0x05, 0xDC]));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].node, Some(2));
        assert_eq!(p.describe(&d[0]), "status n2: state=RUNNING flags=INVERTED supply=24.012 V input_current=1.500 A");

        // 5-byte heartbeat (no current sensor): the missing field is skipped, not shown as 0
        let d = p.decode(&frame(0x101, &[0x03, 3, 0x08, 0x5D, 0xCC]));
        assert_eq!(d[0].values.len(), 3);
        assert!(d[0].values[0].alarm && d[0].values[1].alarm);

        // velocity -12.5 rad/s
        let d = p.decode(&frame(0x101, &[0x02, 0xC1, 0x48, 0, 0]));
        assert_eq!(d[0].values[0].num, Some(-12.5));

        // the shared current frame decodes once per node
        let d = p.decode(&frame(0x010, &[0x3F, 0xC0, 0, 0, 0x7F, 0xC0, 0, 0]));
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].values[0].num, Some(1.5));
        assert_eq!(d[1].values[0].num, None); // NaN = no command

        // encoding: drive node 1 only -> node 2's slot gets its default (NaN)
        let oi = p.outputs.iter().position(|o| o.name.contains("current")).unwrap();
        let f = p.encode_output(oi, &|n| if n == 1 { Some(vec![1.5]) } else { None }).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].id, &f[0].data[..]), (0x010, &[0x3F, 0xC0, 0, 0, 0x7F, 0xC0, 0, 0][..]));
        // drive nodes 1 and 3 -> two frames (0x010 and 0x011)
        let f = p.encode_output(oi, &|n| if n == 1 || n == 3 { Some(vec![0.0]) } else { None }).unwrap();
        assert_eq!(f.iter().map(|f| f.id).collect::<Vec<_>>(), vec![0x010, 0x011]);

        // commands
        let ci = p.commands.iter().position(|c| c.name == "Set velocity interval").unwrap();
        let f = p.build_command(ci, 3, &[20.0]).unwrap();
        assert_eq!((f.id, &f.data[..]), (0x023, &[0x22, 0x10, 0x41, 0xA0, 0, 0][..]));
        let (_, rid, reply, nack) = p.reply_match(ci, 3).unwrap();
        assert_eq!((rid, reply, nack), (0x103, vec![0x22], Some(vec![0xFE, 0x22])));
    }

    #[test]
    fn errors_are_readable() {
        let e = Profile::parse("[[message]]\nname = \"a\"\nid = \"0x100 + n\"\n", "x").err().unwrap();
        assert!(e.contains("[nodes]"), "{e}");
        let e = Profile::parse("[[message]]\nname = \"a\"\nid = 1\nsignals = [{ name = \"x\", byte = 0, type = \"u16\" }]\n", "x").err().unwrap();
        assert!(e.contains("byte order"), "{e}");
    }
}
