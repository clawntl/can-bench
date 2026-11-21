//! Sequences: Rhai scripts from a device profile (`[[sequence]]`), each run in its own thread.
//! They send commands and wait for replies, drive outputs, measure what arrives on the bus and
//! report PASS/FAIL checks. The functions available to scripts are listed in profiles/README.md.

use crate::can::CanFrame;
use crate::device::Cmd;
use crate::profile::{Decoded, Profile};
use rhai::{Array, Dynamic, Engine, EvalAltResult, FnPtr, Map, NativeCallContext, Scope};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type R<T> = Result<T, Box<EvalAltResult>>;
type Key = (Option<i64>, String);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LogKind {
    Text,
    Info,
    Warn,
    Err,
    Pass,
    Fail,
    Prompt,
}

pub enum ScriptEv {
    Log(LogKind, String),
    /// set an output's values for a node and drive it (None = stop driving that node, no stop frame)
    Drive { output: usize, node: i64, values: Option<Vec<f64>> },
    /// stop every output (with stop frames)
    StopAll,
    SetCardNode(i64),
    Confirm(String, Sender<bool>),
    Done { passed: u32, failed: u32, outcome: Outcome },
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Finished,
    Aborted,
    Error(String),
}

#[derive(Clone)]
pub struct Latest {
    pub num: Option<f64>,
    pub text: String,
    pub label: bool,
    pub at: Instant,
}

#[derive(Default, Clone)]
struct Agg {
    count: u64,
    sum: f64,
    n: u64,
    min: f64,
    max: f64,
    last: Option<f64>,
    texts: HashSet<String>,
}

/// What arrived during a `measure(ms)` window.
#[derive(Default, Clone)]
pub struct Window {
    frames: HashMap<Key, u64>,
    sig: HashMap<Key, Agg>,
    bus_errors: u64,
    ms: i64,
}

pub struct Waiter {
    ext: bool,
    id: u32,
    reply: Vec<u8>,
    nack: Option<Vec<u8>>,
    got: Option<(bool, CanFrame)>,
}

/// State the window thread keeps up to date for running scripts.
#[derive(Default)]
pub struct Shared {
    pub signals: HashMap<Key, Latest>,
    pub heard: HashMap<i64, Instant>,
    pub bus_errors: u64,
    /// slider values, (output, node) -> values
    pub outputs: HashMap<(usize, i64), Vec<f64>>,
    windows: Vec<Arc<Mutex<Window>>>,
    waiters: Vec<Arc<Mutex<Waiter>>>,
}

impl Shared {
    /// Called by the window thread for every decoded frame.
    pub fn feed(&mut self, f: &CanFrame, decoded: &[Decoded], p: &Profile, rx: bool, at: Instant) {
        for d in decoded {
            let m = &p.messages[d.message];
            let mut names = vec![m.name.clone()];
            if let Some(v) = d.variant {
                names.push(m.variants[v].name.clone());
            }
            for w in &self.windows {
                let mut w = w.lock().unwrap();
                for nm in &names {
                    *w.frames.entry((d.node, nm.clone())).or_default() += 1;
                }
                for v in &d.values {
                    let a = w.sig.entry((d.node, v.key.clone())).or_default();
                    a.count += 1;
                    a.texts.insert(v.text.clone());
                    if let Some(x) = v.num {
                        if a.n == 0 {
                            a.min = x;
                            a.max = x;
                        }
                        a.n += 1;
                        a.sum += x;
                        a.min = a.min.min(x);
                        a.max = a.max.max(x);
                        a.last = Some(x);
                    }
                }
            }
            for v in &d.values {
                self.signals.insert((d.node, v.key.clone()), Latest { num: v.num, text: v.text.clone(), label: v.label, at });
            }
        }
        if rx {
            for w in &self.waiters {
                let mut w = w.lock().unwrap();
                if w.got.is_none() && w.ext == f.ext && w.id == f.id {
                    if w.nack.as_ref().is_some_and(|n| f.data.starts_with(n)) {
                        w.got = Some((true, f.clone()));
                    } else if f.data.starts_with(&w.reply) {
                        w.got = Some((false, f.clone()));
                    }
                }
            }
        }
    }

    pub fn bus_error(&mut self) {
        self.bus_errors += 1;
        for w in &self.windows {
            w.lock().unwrap().bus_errors += 1;
        }
    }
}

pub struct Ctx {
    pub name: String,
    pub script: String,
    pub profile: Arc<Profile>,
    pub link: Sender<Cmd>,
    pub shared: Arc<Mutex<Shared>>,
    pub abort: Arc<AtomicBool>,
    pub ev: Sender<ScriptEv>,
    pub repaint: eframe::egui::Context,
    pub node: i64,
    pub card_nodes: Vec<i64>,
    pub args: Vec<(String, f64)>,
}

impl Ctx {
    fn emit(&self, e: ScriptEv) {
        let _ = self.ev.send(e);
        self.repaint.request_repaint();
    }

    fn log(&self, k: LogKind, t: String) {
        self.emit(ScriptEv::Log(k, t));
    }

    fn sleep(&self, ms: f64) -> R<()> {
        let end = Instant::now() + Duration::from_secs_f64(ms.max(0.0) / 1000.0);
        loop {
            if self.abort.load(Ordering::Relaxed) {
                return Err("aborted".into());
            }
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            std::thread::sleep((end - now).min(Duration::from_millis(5)));
        }
    }

    fn send_frame(&self, f: CanFrame) -> R<()> {
        self.link.send(Cmd::Send(f)).map_err(|_| "the adapter was closed".into())
    }

    fn command_index(&self, name: &str) -> R<usize> {
        self.profile.commands.iter().position(|c| c.name == name).ok_or_else(|| format!("no command called \"{name}\"").into())
    }

    fn output_index(&self, name: &str) -> R<usize> {
        self.profile.outputs.iter().position(|o| o.name == name).ok_or_else(|| format!("no output called \"{name}\"").into())
    }

    /// Sends a command; with a reply defined, waits for it. Returns the decoded reply as a map,
    /// `()` on timeout or for commands without a reply.
    fn send(&self, cmd: &str, node: i64, args: &[f64], wait: bool) -> R<Dynamic> {
        let ci = self.command_index(cmd)?;
        let f = self.profile.build_command(ci, node, args).map_err(|e| format!("{cmd}: {e}"))?;
        let waiter = match (wait, self.profile.reply_match(ci, node)) {
            (true, Some((ext, id, reply, nack))) => {
                let w = Arc::new(Mutex::new(Waiter { ext, id, reply, nack, got: None }));
                self.shared.lock().unwrap().waiters.push(w.clone());
                Some(w)
            }
            _ => None,
        };
        self.send_frame(f)?;
        let Some(w) = waiter else { return Ok(Dynamic::UNIT) };
        let end = Instant::now() + Duration::from_millis(self.profile.commands[ci].timeout_ms);
        let res = loop {
            if let Some(got) = w.lock().unwrap().got.clone() {
                break Ok(Some(got));
            }
            if Instant::now() >= end {
                break Ok(None);
            }
            if let Err(e) = self.sleep(2.0) {
                break Err(e);
            }
        };
        self.shared.lock().unwrap().waiters.retain(|x| !Arc::ptr_eq(x, &w));
        Ok(match res? {
            None => Dynamic::UNIT,
            Some((nack, frame)) => {
                let mut m = self.frame_map(&frame, node);
                if nack {
                    m.insert("nack".into(), true.into());
                }
                m.into()
            }
        })
    }

    fn frame_map(&self, f: &CanFrame, node: i64) -> Map {
        let mut m = Map::new();
        let decoded = self.profile.decode(f);
        let d = decoded.iter().find(|d| d.node == Some(node)).or(decoded.first());
        if let Some(d) = d {
            for v in &d.values {
                let short = v.key.rsplit('.').next().unwrap_or(&v.key).to_string();
                m.insert(short.into(), value_dyn(v.num, &v.text, v.label));
            }
        }
        m.insert("_frame".into(), f.display().into());
        m
    }

    fn latest(&self, key: &str, node: Option<i64>) -> Option<Latest> {
        let s = self.shared.lock().unwrap();
        s.signals.get(&(node, key.to_string())).or_else(|| s.signals.get(&(None, key.to_string()))).cloned()
    }
}

fn value_dyn(num: Option<f64>, text: &str, label: bool) -> Dynamic {
    if label {
        text.to_string().into()
    } else {
        // whole numbers become integers, so `${r.protocol}` reads "4", not "4.0"
        num.map(|x| if x.fract() == 0.0 && x.abs() < 1e15 { Dynamic::from_int(x as i64) } else { Dynamic::from_float(x) }).unwrap_or(Dynamic::UNIT)
    }
}

fn num(d: &Dynamic) -> Option<f64> {
    d.as_float().ok().or_else(|| d.as_int().ok().map(|i| i as f64))
}

fn nums(d: &Dynamic) -> R<Vec<f64>> {
    if let Some(x) = num(d) {
        return Ok(vec![x]);
    }
    if d.is_array() {
        let a = d.clone().into_array().unwrap_or_default();
        return a.iter().map(|x| num(x).ok_or_else(|| "expected numbers".into())).collect();
    }
    Err("expected a number or an array of numbers".into())
}

fn node_of(d: &Dynamic) -> R<i64> {
    num(d).map(|x| x as i64).ok_or_else(|| "node must be a number".into())
}

/// Syntax check when the profile is loaded.
pub fn check(script: &str) -> Result<(), String> {
    Engine::new().compile(script).map(|_| ()).map_err(|e| e.to_string())
}

pub fn spawn(ctx: Ctx) {
    let name = format!("sequence {}", ctx.name);
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        let passed = Arc::new(AtomicU32::new(0));
        let failed = Arc::new(AtomicU32::new(0));
        let c = Rc::new(ctx);
        let engine = build_engine(c.clone(), passed.clone(), failed.clone());
        let mut scope = Scope::new();
        scope.push_constant("n", c.node);
        scope.push_constant("NAN", f64::NAN);
        scope.push_constant("cards", c.card_nodes.iter().map(|n| Dynamic::from_int(*n)).collect::<Array>());
        for (k, v) in &c.args {
            if v.fract() == 0.0 {
                scope.push(k.clone(), *v as i64);
            } else {
                scope.push(k.clone(), *v);
            }
        }
        let r = engine.run_with_scope(&mut scope, &c.script);
        let outcome = match r {
            Ok(()) => Outcome::Finished,
            Err(_) if c.abort.load(Ordering::Relaxed) => Outcome::Aborted,
            Err(e) => match *e {
                EvalAltResult::ErrorTerminated(..) => Outcome::Aborted,
                EvalAltResult::ErrorRuntime(ref v, pos) if v.is_string() && v.clone().into_string().unwrap_or_default().starts_with("ABORT:") => {
                    let _ = pos;
                    Outcome::Error(v.clone().into_string().unwrap_or_default().trim_start_matches("ABORT:").trim().to_string())
                }
                e => Outcome::Error(e.to_string()),
            },
        };
        c.emit(ScriptEv::Done { passed: passed.load(Ordering::Relaxed), failed: failed.load(Ordering::Relaxed), outcome });
    });
}

fn build_engine(c: Rc<Ctx>, passed: Arc<AtomicU32>, failed: Arc<AtomicU32>) -> Engine {
    let mut e = Engine::new();
    let ab = c.abort.clone();
    e.on_progress(move |_| if ab.load(Ordering::Relaxed) { Some(Dynamic::UNIT) } else { None });
    {
        let c = c.clone();
        e.on_print(move |s| c.log(LogKind::Text, s.to_string()));
    }
    {
        let c = c.clone();
        e.on_debug(move |s, _, _| c.log(LogKind::Text, s.to_string()));
    }

    // ---- time
    let cc = c.clone();
    e.register_fn("wait", move |ms: Dynamic| -> R<()> { cc.sleep(num(&ms).ok_or("wait(ms): ms must be a number")?) });
    let cc = c.clone();
    e.register_fn("wait_until", move |ctx: NativeCallContext, f: FnPtr, timeout: Dynamic| -> R<bool> {
        let end = Instant::now() + Duration::from_secs_f64(num(&timeout).unwrap_or(1000.0) / 1000.0);
        loop {
            let r: Dynamic = f.call_within_context(&ctx, ())?;
            if r.as_bool().unwrap_or(false) {
                return Ok(true);
            }
            if Instant::now() >= end {
                return Ok(false);
            }
            cc.sleep(5.0)?;
        }
    });

    // ---- commands and frames
    let cc = c.clone();
    e.register_fn("send", move |cmd: &str| -> R<Dynamic> { cc.send(cmd, cc.node, &[], true) });
    let cc = c.clone();
    e.register_fn("send", move |cmd: &str, node: Dynamic| -> R<Dynamic> { cc.send(cmd, node_of(&node)?, &[], true) });
    let cc = c.clone();
    e.register_fn("send", move |cmd: &str, node: Dynamic, args: Dynamic| -> R<Dynamic> { cc.send(cmd, node_of(&node)?, &nums(&args)?, true) });
    let cc = c.clone();
    e.register_fn("send_only", move |cmd: &str, node: Dynamic| -> R<()> { cc.send(cmd, node_of(&node)?, &[], false).map(|_| ()) });
    let cc = c.clone();
    e.register_fn("send_only", move |cmd: &str, node: Dynamic, args: Dynamic| -> R<()> { cc.send(cmd, node_of(&node)?, &nums(&args)?, false).map(|_| ()) });
    let cc = c.clone();
    e.register_fn("send_raw", move |id: i64, hex: &str| -> R<()> {
        let data = crate::can::parse_hex_bytes(hex).map_err(|e| e.to_string())?;
        cc.send_frame(raw_frame(id, data))
    });
    let cc = c.clone();
    e.register_fn("repeat_raw", move |id: i64, hex: &str, period: Dynamic, duration: Dynamic| -> R<()> {
        let data = crate::can::parse_hex_bytes(hex).map_err(|e| e.to_string())?;
        let period = num(&period).unwrap_or(10.0).max(1.0);
        let end = Instant::now() + Duration::from_secs_f64(num(&duration).unwrap_or(0.0) / 1000.0);
        while Instant::now() < end {
            cc.send_frame(raw_frame(id, data.clone()))?;
            cc.sleep(period)?;
        }
        Ok(())
    });

    // ---- values
    let cc = c.clone();
    e.register_fn("value", move |key: &str| -> Dynamic { cc.latest(key, Some(cc.node)).map(|l| value_dyn(l.num, &l.text, l.label)).unwrap_or(Dynamic::UNIT) });
    let cc = c.clone();
    e.register_fn("value", move |key: &str, node: i64| -> Dynamic { cc.latest(key, Some(node)).map(|l| value_dyn(l.num, &l.text, l.label)).unwrap_or(Dynamic::UNIT) });
    let cc = c.clone();
    e.register_fn("text", move |key: &str, node: i64| -> String { cc.latest(key, Some(node)).map(|l| l.text).unwrap_or_default() });
    let cc = c.clone();
    e.register_fn("text", move |key: &str| -> String { cc.latest(key, Some(cc.node)).map(|l| l.text).unwrap_or_default() });
    let cc = c.clone();
    e.register_fn("age", move |key: &str, node: i64| -> i64 { cc.latest(key, Some(node)).map(|l| l.at.elapsed().as_millis() as i64).unwrap_or(i64::MAX) });
    let cc = c.clone();
    e.register_fn("heard", move |node: i64, ms: Dynamic| -> bool {
        let ms = num(&ms).unwrap_or(1000.0);
        cc.shared.lock().unwrap().heard.get(&node).is_some_and(|t| t.elapsed().as_secs_f64() * 1000.0 <= ms)
    });
    let cc = c.clone();
    e.register_fn("nodes_heard", move |ms: Dynamic| -> Array {
        let ms = num(&ms).unwrap_or(1000.0);
        let s = cc.shared.lock().unwrap();
        let mut v: Vec<i64> = s.heard.iter().filter(|(_, t)| t.elapsed().as_secs_f64() * 1000.0 <= ms).map(|(n, _)| *n).collect();
        v.sort();
        v.into_iter().map(Dynamic::from_int).collect()
    });
    let cc = c.clone();
    e.register_fn("bus_errors", move || -> i64 { cc.shared.lock().unwrap().bus_errors as i64 });

    // ---- measuring
    let cc = c.clone();
    e.register_fn("measure", move |ms: Dynamic| -> R<Window> {
        let ms = num(&ms).ok_or("measure(ms): ms must be a number")?;
        let w = Arc::new(Mutex::new(Window { ms: ms as i64, ..Default::default() }));
        cc.shared.lock().unwrap().windows.push(w.clone());
        let r = cc.sleep(ms);
        cc.shared.lock().unwrap().windows.retain(|x| !Arc::ptr_eq(x, &w));
        r?;
        let out = w.lock().unwrap().clone();
        Ok(out)
    });
    e.register_type_with_name::<Window>("Measurement");
    // start_measure() … m.stop(): a window around a longer stretch of script
    let cc = c.clone();
    e.register_fn("start_measure", move || -> Running {
        let w = Arc::new(Mutex::new(Window::default()));
        cc.shared.lock().unwrap().windows.push(w.clone());
        Running(w, Instant::now(), cc.shared.clone())
    });
    e.register_type_with_name::<Running>("RunningMeasurement");
    e.register_fn("stop", |r: &mut Running| -> Window {
        r.2.lock().unwrap().windows.retain(|x| !Arc::ptr_eq(x, &r.0));
        let mut w = r.0.lock().unwrap().clone();
        w.ms = r.1.elapsed().as_millis() as i64;
        w
    });
    e.register_fn("count", |w: &mut Window, name: &str, node: i64| -> i64 { w.count(name, Some(node)) });
    e.register_fn("count", |w: &mut Window, name: &str| -> i64 { w.count(name, None) });
    e.register_fn("avg", |w: &mut Window, key: &str, node: i64| -> Dynamic { w.agg(key, node, |a| a.sum / a.n as f64) });
    e.register_fn("min", |w: &mut Window, key: &str, node: i64| -> Dynamic { w.agg(key, node, |a| a.min) });
    e.register_fn("max", |w: &mut Window, key: &str, node: i64| -> Dynamic { w.agg(key, node, |a| a.max) });
    e.register_fn("last", |w: &mut Window, key: &str, node: i64| -> Dynamic { w.agg(key, node, |a| a.last.unwrap_or(f64::NAN)) });
    e.register_fn("seen", |w: &mut Window, key: &str, node: i64, text: &str| -> bool {
        w.sig.get(&(Some(node), key.to_string())).is_some_and(|a| a.texts.iter().any(|t| t == text || t.split('|').any(|p| p == text)))
    });
    e.register_fn("bus_errors", |w: &mut Window| -> i64 { w.bus_errors as i64 });
    e.register_get("ms", |w: &mut Window| -> i64 { w.ms });

    // ---- outputs
    let cc = c.clone();
    e.register_fn("drive", move |output: &str, node: Dynamic, value: Dynamic| -> R<()> {
        let o = cc.output_index(output)?;
        cc.emit(ScriptEv::Drive { output: o, node: node_of(&node)?, values: Some(nums(&value)?) });
        cc.sleep(1.0)
    });
    let cc = c.clone();
    e.register_fn("release", move |output: &str, node: Dynamic| -> R<()> {
        let o = cc.output_index(output)?;
        cc.emit(ScriptEv::Drive { output: o, node: node_of(&node)?, values: None });
        cc.sleep(1.0)
    });
    let cc = c.clone();
    e.register_fn("stop", move || -> R<()> {
        cc.emit(ScriptEv::StopAll);
        cc.sleep(1.0)
    });
    let cc = c.clone();
    e.register_fn("slider", move |output: &str, node: Dynamic| -> R<f64> {
        let o = cc.output_index(output)?;
        let n = node_of(&node)?;
        Ok(cc.shared.lock().unwrap().outputs.get(&(o, n)).and_then(|v| v.first().copied()).unwrap_or(0.0))
    });

    // ---- reporting
    let (cc, p, f) = (c.clone(), passed.clone(), failed.clone());
    e.register_fn("check", move |name: &str, ok: bool, detail: Dynamic| -> bool {
        let d = detail.to_string();
        let d = if d.is_empty() || detail.is_unit() { String::new() } else { format!(" — {d}") };
        if ok {
            p.fetch_add(1, Ordering::Relaxed);
            cc.log(LogKind::Pass, format!("PASS  {name}{d}"));
        } else {
            f.fetch_add(1, Ordering::Relaxed);
            cc.log(LogKind::Fail, format!("FAIL  {name}{d}"));
        }
        ok
    });
    let (cc, p, f) = (c.clone(), passed.clone(), failed.clone());
    e.register_fn("check", move |name: &str, ok: bool| -> bool {
        if ok {
            p.fetch_add(1, Ordering::Relaxed);
            cc.log(LogKind::Pass, format!("PASS  {name}"));
        } else {
            f.fetch_add(1, Ordering::Relaxed);
            cc.log(LogKind::Fail, format!("FAIL  {name}"));
        }
        ok
    });
    for (fname, kind) in [("log", LogKind::Text), ("info", LogKind::Info), ("warn", LogKind::Warn), ("error", LogKind::Err), ("prompt", LogKind::Prompt)] {
        let cc = c.clone();
        e.register_fn(fname, move |t: Dynamic| cc.log(kind, t.to_string()));
    }
    e.register_fn("abort", |t: Dynamic| -> R<()> { Err(format!("ABORT: {t}").into()) });
    let cc = c.clone();
    e.register_fn("confirm", move |t: Dynamic| -> R<bool> {
        let (tx, rx) = channel();
        cc.emit(ScriptEv::Confirm(t.to_string(), tx));
        loop {
            if let Ok(v) = rx.try_recv() {
                return Ok(v);
            }
            cc.sleep(20.0)?;
        }
    });
    let cc = c.clone();
    e.register_fn("set_node", move |n: Dynamic| -> R<()> {
        cc.emit(ScriptEv::SetCardNode(node_of(&n)?));
        Ok(())
    });
    e.register_fn("fmt", |x: Dynamic, decimals: i64| -> String {
        match num(&x) {
            Some(v) => format!("{v:.*}", decimals.clamp(0, 9) as usize),
            None => x.to_string(),
        }
    });
    e.register_fn("abs", |x: Dynamic| -> Dynamic { num(&x).map(|v| Dynamic::from_float(v.abs())).unwrap_or(x) });
    e.register_fn("max", |a: Dynamic, b: Dynamic| -> f64 { num(&a).unwrap_or(f64::NAN).max(num(&b).unwrap_or(f64::NAN)) });
    e.register_fn("min", |a: Dynamic, b: Dynamic| -> f64 { num(&a).unwrap_or(f64::NAN).min(num(&b).unwrap_or(f64::NAN)) });
    e.register_fn("is_nan", |x: Dynamic| -> bool { num(&x).is_none_or(|v| v.is_nan()) });
    e.register_fn("is_none", |x: Dynamic| -> bool { x.is_unit() });
    e
}

#[derive(Clone)]
pub struct Running(Arc<Mutex<Window>>, Instant, Arc<Mutex<Shared>>);

fn raw_frame(id: i64, data: Vec<u8>) -> CanFrame {
    let id = id.clamp(0, 0x1FFF_FFFF) as u32;
    CanFrame { id, ext: id > 0x7FF, rtr: false, fd: data.len() > 8, brs: false, esi: false, data }
}

impl Window {
    fn count(&self, name: &str, node: Option<i64>) -> i64 {
        let k = (node, name.to_string());
        if name.contains('.') {
            self.sig.get(&k).map(|a| a.count as i64).unwrap_or(0)
        } else {
            self.frames.get(&k).copied().unwrap_or(0) as i64
        }
    }

    fn agg(&self, key: &str, node: i64, f: impl Fn(&Agg) -> f64) -> Dynamic {
        match self.sig.get(&(Some(node), key.to_string())).or_else(|| self.sig.get(&(None, key.to_string()))) {
            Some(a) if a.n > 0 => Dynamic::from_float(f(a)),
            _ => Dynamic::UNIT,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn syntax_check() {
        assert!(super::check("let x = 1; if x > 0 { log(\"ok\") }").is_ok());
        assert!(super::check("let x = ;").is_err());
    }
}
