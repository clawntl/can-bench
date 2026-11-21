//! Device page state and logic: the loaded profile, decoded signals, nodes, outputs, commands,
//! sequences (scripts) and the dashboard layout. The page itself is drawn in `dash.rs`.

use super::{App, Kind};
use crate::can::CanFrame;
use crate::device::Cmd;
use crate::periodic::Job;
use crate::profile::{Profile, Value, WidgetDef};
use crate::script::{self, LogKind, Outcome, ScriptEv, Shared};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Tx tags at or above this belong to profile outputs (below: rows of the periodic list).
pub const OUTPUT_TAG: usize = 1 << 20;
pub(super) const HISTORY_POINTS: usize = 20_000;
pub(super) const HISTORY_SECS: f64 = 600.0;
pub(super) const BUILTINS: [(&str, &str); 8] = [
    ("@nodes", "Nodes"),
    ("@signals", "Signals"),
    ("@plot", "Plot"),
    ("@outputs", "Outputs"),
    ("@commands", "Commands"),
    ("@sequences", "Sequences"),
    ("@log", "Log"),
    ("@bus", "Bus"),
];

pub(super) type SigKey = (Option<i64>, String);

pub(super) struct Sig {
    pub text: String,
    pub num: Option<f64>,
    pub alarm: bool,
    pub last: Instant,
    pub tx: bool,
    pub hist: VecDeque<[f64; 2]>,
}

#[derive(Default)]
pub(super) struct RateStat {
    pub times: VecDeque<Instant>,
    pub last: Option<Instant>,
    pub max_gap_ms: f64,
    /// gaps longer than 50 ms (ms), for "gaps > N ms" counts
    pub gaps: Vec<f64>,
}

pub(super) struct Pending {
    pub name: String,
    pub node: i64,
    pub ext: bool,
    pub id: u32,
    pub reply: Vec<u8>,
    pub nack: Option<Vec<u8>>,
    pub deadline: Instant,
}

pub struct OutState {
    /// nodes this output drives ("Send" ticked); node 0 for messages without n
    pub nodes: BTreeSet<i64>,
    /// slider values per node
    pub values: BTreeMap<i64, Vec<f64>>,
    pub sent: u64,
}

pub(super) struct LogLine {
    pub t: String,
    pub kind: Kind,
    pub text: String,
}

pub(super) enum Confirm {
    Command(usize, i64),
    Sequence(usize, i64, Option<String>),
    Script(String, Sender<bool>),
    Text(String, Box<Confirm>),
}

pub(super) struct Running {
    pub name: String,
    pub abort: Arc<AtomicBool>,
    pub rx: Receiver<ScriptEv>,
    pub stop_after: bool,
    pub card: Option<String>,
}

pub struct Edit {
    pub before: (Vec<Vec<String>>, Vec<f32>),
    pub drag: Option<String>,
}

pub struct DevState {
    pub profile: Option<Arc<Profile>>,
    pub path: Option<PathBuf>,
    pub load_err: Option<String>,
    pub decode_trace: bool,
    pub(super) signals: BTreeMap<SigKey, Sig>,
    pub(super) heard: BTreeMap<i64, Instant>,
    pub(super) alive_prev: BTreeMap<i64, bool>,
    pub(super) counts: HashMap<SigKey, u64>,
    pub(super) rates: HashMap<SigKey, RateStat>,
    pub(super) bus_errors: u64,
    pub(super) sel_node: i64,
    pub(super) args: Vec<Vec<f64>>,
    pub(super) seq_args: Vec<Vec<f64>>,
    pub(super) pending: Vec<Pending>,
    pub(super) log: Vec<LogLine>,
    pub outputs: Vec<OutState>,
    pub(super) linked: BTreeSet<usize>,
    pub(super) plot: BTreeSet<SigKey>,
    pub(super) plot_secs: f64,
    pub(super) follow: bool,
    pub(super) t0: Instant,
    /// plot time while the adapter is closed (the plots stand still)
    pub(super) frozen_at: Option<f64>,
    pub(super) confirm: Option<Confirm>,
    pub(super) filter: String,
    // dashboard
    pub(super) layout: Vec<Vec<String>>,
    pub(super) widths: Vec<f32>,
    pub(super) card_nodes: HashMap<String, i64>,
    pub(super) popups: Vec<(String, i64, Option<String>)>,
    pub(super) collapsed: HashSet<String>,
    pub(super) edit: Option<Edit>,
    // derived signals and scripts
    engine: rhai::Engine,
    derived: Vec<rhai::AST>,
    pub(super) shared: Arc<Mutex<Shared>>,
    pub(super) running: Option<Running>,
    pub(super) active_ids: HashSet<(bool, u32)>,
    pub(super) foreign: HashMap<u32, Instant>,
}

fn profile_setting() -> PathBuf {
    super::own_firmware_dir().parent().map(|p| p.join("profile.txt")).unwrap_or_else(|| PathBuf::from("profile.txt"))
}

impl DevState {
    pub fn new() -> Self {
        let mut s = DevState {
            profile: None,
            path: None,
            load_err: None,
            decode_trace: true,
            signals: BTreeMap::new(),
            heard: BTreeMap::new(),
            alive_prev: BTreeMap::new(),
            counts: HashMap::new(),
            rates: HashMap::new(),
            bus_errors: 0,
            sel_node: 1,
            args: Vec::new(),
            seq_args: Vec::new(),
            pending: Vec::new(),
            log: Vec::new(),
            outputs: Vec::new(),
            linked: BTreeSet::new(),
            plot: BTreeSet::new(),
            plot_secs: 10.0,
            follow: true,
            t0: Instant::now(),
            frozen_at: None,
            confirm: None,
            filter: String::new(),
            layout: Vec::new(),
            widths: Vec::new(),
            card_nodes: HashMap::new(),
            popups: Vec::new(),
            collapsed: HashSet::new(),
            edit: None,
            engine: rhai::Engine::new(),
            derived: Vec::new(),
            shared: Arc::new(Mutex::new(Shared::default())),
            running: None,
            active_ids: HashSet::new(),
            foreign: HashMap::new(),
        };
        s.default_layout();
        if let Ok(p) = std::fs::read_to_string(profile_setting()) {
            let p = PathBuf::from(p.trim());
            if p.is_file() {
                s.load(p);
            }
        }
        s
    }

    /// Loads (or reloads) a profile. Keeps slider values and card nodes when the names still match.
    pub fn load(&mut self, path: PathBuf) {
        let parsed = Profile::load(&path).and_then(|p| {
            let mut asts = Vec::new();
            for d in &p.derived {
                asts.push(self.engine.compile_expression(&d.expr).map_err(|e| format!("derived \"{}\": {e}", d.name))?);
            }
            for s in &p.sequences {
                script::check(&s.script).map_err(|e| format!("sequence \"{}\": {e}", s.name))?;
            }
            Ok((p, asts))
        });
        match parsed {
            Ok((p, asts)) => {
                let old_names: Vec<String> = self.profile.as_ref().map(|o| o.outputs.iter().map(|x| x.name.clone()).collect()).unwrap_or_default();
                let old = std::mem::take(&mut self.outputs);
                self.outputs = p
                    .outputs
                    .iter()
                    .map(|o| OutState {
                        // never drive anything by itself after a (re)load
                        nodes: BTreeSet::new(),
                        values: old_names.iter().position(|n| n == &o.name).and_then(|i| old.get(i)).map(|x| x.values.clone()).unwrap_or_default(),
                        sent: 0,
                    })
                    .collect();
                self.args = p.commands.iter().map(|c| c.args.iter().map(|a| a.1).collect()).collect();
                self.seq_args = p.sequences.iter().map(|s| s.args.iter().map(|a| a.1).collect()).collect();
                if let Some(n) = &p.nodes {
                    self.sel_node = self.sel_node.clamp(n.min, n.max);
                }
                self.card_nodes.retain(|t, _| p.cards.iter().any(|c| &c.title == t));
                self.popups.retain(|(t, _, _)| p.cards.iter().any(|c| &c.title == t));
                self.linked.clear();
                self.pending.clear();
                self.derived = asts;
                self.load_err = None;
                self.profile = Some(Arc::new(p));
                self.edit = None;
                self.default_layout();
                let _ = std::fs::write(profile_setting(), path.display().to_string());
                self.path = Some(path);
            }
            Err(e) => {
                self.load_err = Some(e);
                self.path = Some(path);
            }
        }
    }

    pub fn unload(&mut self) {
        self.profile = None;
        self.outputs.clear();
        self.pending.clear();
        self.load_err = None;
        self.path = None;
        self.edit = None;
        self.popups.clear();
        self.default_layout();
        let _ = std::fs::remove_file(profile_setting());
    }

    /// The profile's layout, or a sensible one built from its cards.
    pub(super) fn default_layout(&mut self) {
        let p = self.profile.clone();
        let (cols, widths) = match p.as_deref() {
            Some(Profile { layout: Some(l), .. }) => (l.columns.clone(), l.widths.clone()),
            Some(p) if p.cards.iter().any(|c| !c.popup) => {
                let mut cols = vec![Vec::new(), Vec::new()];
                for (i, c) in p.cards.iter().filter(|c| !c.popup).enumerate() {
                    cols[i % 2].push(c.title.clone());
                }
                let has_log = p.cards.iter().any(|c| c.widgets.iter().any(|w| matches!(w, WidgetDef::Log { .. })));
                if !has_log {
                    cols[1].push("@log".into());
                }
                (cols, vec![1.0, 1.0])
            }
            other => {
                let has = |f: &dyn Fn(&Profile) -> bool| other.is_some_and(f);
                let mut left: Vec<String> = Vec::new();
                if has(&|p| p.nodes.is_some()) {
                    left.push("@nodes".into());
                }
                if has(&|p| !p.outputs.is_empty()) {
                    left.push("@outputs".into());
                }
                if has(&|p| !p.commands.is_empty()) {
                    left.push("@commands".into());
                }
                if has(&|p| !p.sequences.is_empty()) {
                    left.push("@sequences".into());
                }
                let right: Vec<String> = vec!["@signals".into(), "@plot".into(), "@log".into()];
                if left.is_empty() {
                    (vec![right], vec![1.0])
                } else {
                    (vec![left, right], vec![1.0, 1.25])
                }
            }
        };
        let n = cols.len().max(1);
        self.widths = (0..n).map(|i| widths.get(i).copied().filter(|w| *w > 0.05).unwrap_or(1.0)).collect();
        self.layout = if cols.is_empty() { vec![Vec::new()] } else { cols };
    }

    /// The node a card is showing.
    pub(super) fn card_node(&self, title: &str) -> i64 {
        if let Some(n) = self.card_nodes.get(title) {
            return *n;
        }
        let p = self.profile.as_ref();
        p.and_then(|p| p.cards.iter().find(|c| c.title == title)).and_then(|c| c.node).or_else(|| p.and_then(|p| p.nodes.as_ref().map(|n| n.min))).unwrap_or(0)
    }

    /// Nodes of every (non-popup) card that has a node.
    pub(super) fn card_node_list(&self) -> Vec<i64> {
        let Some(p) = &self.profile else { return Vec::new() };
        let mut v: Vec<i64> = p.cards.iter().filter(|c| !c.popup && (c.node.is_some() || c.node_select)).map(|c| self.card_node(&c.title)).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Nodes whose cards have a slider for this output (for "link").
    pub(super) fn slider_nodes(&self, oi: usize) -> Vec<i64> {
        let Some(p) = &self.profile else { return Vec::new() };
        let name = &p.outputs[oi].name;
        fn has(ws: &[WidgetDef], name: &str) -> bool {
            ws.iter().any(|w| match w {
                WidgetDef::Slider { output, .. } => output == name,
                WidgetDef::Row { widgets } => has(widgets, name),
                _ => false,
            })
        }
        let mut v: Vec<i64> = p.cards.iter().filter(|c| !c.popup && has(&c.widgets, name)).map(|c| self.card_node(&c.title)).collect();
        v.sort();
        v.dedup();
        v
    }

    pub(super) fn is_alive(&self, n: i64) -> bool {
        let timeout = self.profile.as_ref().and_then(|p| p.nodes.as_ref()).map(|x| x.timeout_ms).unwrap_or(1000);
        self.heard.get(&n).is_some_and(|t| t.elapsed() < Duration::from_millis(timeout))
    }

    fn derive(&self, p: &Profile, d: &mut crate::profile::Decoded) {
        let m = &p.messages[d.message];
        let names = [Some(m.name.as_str()), d.variant.map(|v| m.variants[v].name.as_str())];
        for (i, def) in p.derived.iter().enumerate() {
            if !names.contains(&Some(def.on.as_str())) {
                continue;
            }
            let mut scope = rhai::Scope::new();
            let mut complete = true;
            for v in &d.values {
                let short = v.key.rsplit('.').next().unwrap_or(&v.key).to_string();
                match v.num {
                    Some(x) => {
                        scope.push(short, x);
                    }
                    None => complete = false,
                }
            }
            let r = if complete { self.engine.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &self.derived[i]).ok() } else { None };
            let num = r.and_then(|r| r.as_float().ok().or_else(|| r.as_int().ok().map(|x| x as f64)));
            let dec = def.decimals.unwrap_or(3);
            let text = match num {
                Some(x) if def.unit.is_empty() => format!("{x:.dec$}"),
                Some(x) => format!("{x:.dec$} {}", def.unit),
                None => "–".into(),
            };
            let alarm = num.is_some_and(|x| def.alarm_below.is_some_and(|b| x < b) || def.alarm_above.is_some_and(|a| x > a));
            d.values.push(Value { key: format!("{}.{}", def.on, def.name), num, text, alarm, label: false, log_changes: false });
        }
    }

    /// Decodes a frame seen on the bus (`rx`) or sent by us. Updates signals, nodes, rates and
    /// command replies; returns the text for the trace (empty when nothing matched) and log lines.
    pub fn on_frame(&mut self, f: &CanFrame, at: Instant, rx: bool) -> (String, Vec<(Kind, String)>) {
        let Some(p) = self.profile.clone() else { return (String::new(), Vec::new()) };
        let mut decoded = p.decode(f);
        for d in &mut decoded {
            self.derive(&p, d);
        }
        let mut results = Vec::new();
        if rx {
            if self.active_ids.contains(&(f.ext, f.id)) {
                let last = self.foreign.insert(f.id, at);
                if last.is_none_or(|t| at.saturating_duration_since(t) > Duration::from_secs(3)) {
                    results.push((Kind::Warn, format!("Another node is sending {} too — it fights your output.", f.display().split(':').next().unwrap_or(""))));
                }
            }
            let mut i = 0;
            while i < self.pending.len() {
                let q = &self.pending[i];
                if q.ext == f.ext && q.id == f.id {
                    let what = decoded.iter().map(|d| p.describe(d)).collect::<Vec<_>>().join(" | ");
                    let what = if what.is_empty() { f.display() } else { what };
                    if q.nack.as_ref().is_some_and(|n| f.data.starts_with(n)) {
                        results.push((Kind::Err, format!("{} (node {}) refused: {what}", q.name, q.node)));
                        self.pending.remove(i);
                        continue;
                    }
                    if f.data.starts_with(&q.reply) {
                        results.push((Kind::Info, format!("{} (node {}) → {what}", q.name, q.node)));
                        self.pending.remove(i);
                        continue;
                    }
                }
                i += 1;
            }
        }
        let t = at.saturating_duration_since(self.t0).as_secs_f64();
        let mut texts = Vec::new();
        let mut heard = None;
        for d in &decoded {
            texts.push(p.describe(d));
            let m = &p.messages[d.message];
            if rx {
                let mut names = vec![m.name.clone()];
                if let Some(v) = d.variant {
                    names.push(m.variants[v].name.clone());
                }
                for nm in names {
                    let k = (d.node, nm);
                    *self.counts.entry(k.clone()).or_default() += 1;
                    let r = self.rates.entry(k).or_default();
                    if let Some(last) = r.last {
                        let gap = at.saturating_duration_since(last).as_secs_f64() * 1000.0;
                        r.max_gap_ms = r.max_gap_ms.max(gap);
                        if gap > 50.0 && r.gaps.len() < 100_000 {
                            r.gaps.push(gap);
                        }
                    }
                    r.last = Some(at);
                    r.times.push_back(at);
                    while r.times.front().is_some_and(|x| at.saturating_duration_since(*x) > Duration::from_secs(1)) {
                        r.times.pop_front();
                    }
                }
                if let (Some(n), Some(nodes)) = (d.node, &p.nodes) {
                    let is_hb = match nodes.heartbeat {
                        None => true,
                        Some((mi, v)) => mi == d.message && (v.is_none() || v == d.variant),
                    };
                    if is_hb {
                        self.heard.insert(n, at);
                        heard = Some(n);
                    }
                }
            }
            for v in &d.values {
                let key = (d.node, v.key.clone());
                if rx && v.log_changes {
                    let old = self.signals.get(&key).map(|s| s.text.clone());
                    if old.as_deref() != Some(v.text.as_str()) {
                        let who = d.node.map(|n| format!("node {n}: ")).unwrap_or_default();
                        let text = match old {
                            Some(o) => format!("{who}{} {o} → {}", v.key, v.text),
                            None => format!("{who}{} {}", v.key, v.text),
                        };
                        results.push((if v.alarm { Kind::Err } else { Kind::Info }, text));
                    }
                }
                let s = self.signals.entry(key).or_insert_with(|| Sig { text: String::new(), num: None, alarm: false, last: at, tx: !rx, hist: VecDeque::new() });
                s.text = v.text.clone();
                s.num = v.num;
                s.alarm = v.alarm;
                s.last = at;
                s.tx = !rx;
                if let Some(x) = v.num {
                    s.hist.push_back([t, x]);
                    while s.hist.len() > HISTORY_POINTS || s.hist.front().is_some_and(|h| h[0] < t - HISTORY_SECS) {
                        s.hist.pop_front();
                    }
                }
            }
        }
        {
            let mut sh = self.shared.lock().unwrap();
            sh.feed(f, &decoded, &p, rx, at);
            if let Some(n) = heard {
                sh.heard.insert(n, at);
            }
        }
        (texts.join(" | "), results)
    }

    /// Command replies that did not come in time, and nodes that went missing / came back.
    pub(super) fn tick(&mut self) -> Vec<(Kind, String)> {
        let now = Instant::now();
        let mut out = Vec::new();
        self.pending.retain(|q| {
            if now >= q.deadline {
                out.push((Kind::Warn, format!("{} (node {}): no reply", q.name, q.node)));
                false
            } else {
                true
            }
        });
        let label = self.profile.as_ref().and_then(|p| p.nodes.as_ref()).map(|n| n.label.clone()).unwrap_or_else(|| "Node".into());
        let timeout = self.profile.as_ref().and_then(|p| p.nodes.as_ref()).map(|n| n.timeout_ms).unwrap_or(1000);
        let nodes: Vec<i64> = self.heard.keys().copied().collect();
        for n in nodes {
            let alive = self.is_alive(n);
            let prev = self.alive_prev.insert(n, alive);
            if prev != Some(alive) {
                if alive {
                    out.push((Kind::Info, format!("{label} {n} online")));
                } else {
                    out.push((Kind::Warn, format!("{label} {n} OFFLINE (no heartbeat for {:.1} s)", timeout as f64 / 1000.0)));
                }
            }
        }
        out
    }

    pub(super) fn values_for(&self, oi: usize, n: i64) -> Vec<f64> {
        let p = self.profile.as_ref().unwrap();
        let m = &p.messages[p.outputs[oi].message];
        self.outputs[oi].values.get(&n).cloned().unwrap_or_else(|| m.signals.iter().map(|s| s.stop.or(s.default).filter(|v| !v.is_nan()).unwrap_or(0.0)).collect())
    }

    /// Periodic jobs for every output that drives at least one node.
    pub fn jobs(&mut self) -> (Vec<Job>, Vec<String>) {
        let mut jobs = Vec::new();
        let mut errs = Vec::new();
        self.active_ids.clear();
        let Some(p) = self.profile.clone() else { return (jobs, errs) };
        for (oi, o) in p.outputs.iter().enumerate() {
            if self.outputs[oi].nodes.is_empty() {
                continue;
            }
            let vals: BTreeMap<i64, Vec<f64>> = self.outputs[oi].nodes.iter().map(|&n| (n, self.values_for(oi, n))).collect();
            match p.encode_output(oi, &|n| vals.get(&n).cloned()) {
                Ok(frames) => {
                    for frame in frames {
                        self.active_ids.insert((frame.ext, frame.id));
                        jobs.push(Job { tag: OUTPUT_TAG + oi, frame, period: Duration::from_millis(o.period_ms) });
                    }
                }
                Err(e) => {
                    errs.push(format!("{}: {e}", o.name));
                    self.outputs[oi].nodes.clear();
                }
            }
        }
        let mut sh = self.shared.lock().unwrap();
        sh.outputs.clear();
        for (oi, o) in self.outputs.iter().enumerate() {
            for (n, v) in &o.values {
                sh.outputs.insert((oi, *n), v.clone());
            }
        }
        (jobs, errs)
    }

    /// One last frame per output, with each signal's `stop` value for the nodes it was driving.
    /// Outputs without any `stop` value send nothing.
    pub fn stop_frames(&self, only: Option<usize>) -> Vec<CanFrame> {
        let Some(p) = &self.profile else { return Vec::new() };
        let mut out = Vec::new();
        for (oi, o) in p.outputs.iter().enumerate() {
            if self.outputs[oi].nodes.is_empty() || only.is_some_and(|x| x != oi) {
                continue;
            }
            let m = &p.messages[o.message];
            if m.signals.iter().all(|s| s.stop.is_none()) {
                continue;
            }
            let stop: Vec<f64> = m.signals.iter().map(|s| s.stop.or(s.default).unwrap_or(0.0)).collect();
            let driven = &self.outputs[oi].nodes;
            if let Ok(f) = p.encode_output(oi, &|n| if driven.contains(&n) { Some(stop.clone()) } else { None }) {
                out.extend(f);
            }
        }
        out
    }

    pub fn any_output_on(&self) -> bool {
        self.outputs.iter().any(|o| !o.nodes.is_empty()) || self.running.is_some()
    }

    /// Seconds since `t0` for the plots: running while the adapter is open, frozen while closed.
    pub(super) fn plot_now(&mut self, open: bool) -> f64 {
        let t = self.t0.elapsed().as_secs_f64();
        if open {
            self.frozen_at = None;
            t
        } else {
            *self.frozen_at.get_or_insert(t)
        }
    }

    pub(super) fn reset_seen(&mut self) {
        self.signals.clear();
        self.heard.clear();
        self.alive_prev.clear();
        self.counts.clear();
        self.rates.clear();
        self.t0 = Instant::now();
        self.frozen_at = None;
    }
}

impl App {
    /// Sends the stop frames of all outputs that drive something (or one), then stops them.
    pub(super) fn stop_outputs(&mut self, only: Option<usize>) {
        let frames = self.dev.stop_frames(only);
        for (i, o) in self.dev.outputs.iter_mut().enumerate() {
            if only.is_none_or(|x| x == i) {
                o.nodes.clear();
            }
        }
        self.sync_periodic();
        if let Some(l) = &self.link {
            for f in frames {
                let _ = l.tx.send(Cmd::Send(f));
            }
        }
    }

    /// Esc / STOP: abort the running sequence and stop every output.
    pub(super) fn stop_everything(&mut self) {
        self.abort_sequence();
        self.stop_outputs(None);
    }

    pub(super) fn abort_sequence(&mut self) {
        if let Some(r) = &self.dev.running {
            r.abort.store(true, Ordering::Relaxed);
        }
    }

    /// Sets a slider (and optionally Send) for a node; with Link on, for every linked node too.
    pub(super) fn set_output(&mut self, oi: usize, n: i64, values: Option<Vec<f64>>, drive: Option<bool>) {
        let mut nodes = vec![n];
        if self.dev.linked.contains(&oi) {
            for x in self.dev.slider_nodes(oi) {
                if !nodes.contains(&x) {
                    nodes.push(x);
                }
            }
        }
        let was_driven: Vec<i64> = self.dev.outputs[oi].nodes.iter().copied().collect();
        for x in &nodes {
            let o = &mut self.dev.outputs[oi];
            if let Some(v) = &values {
                o.values.insert(*x, v.clone());
            }
            match drive {
                Some(true) => {
                    if o.nodes.is_empty() {
                        o.sent = 0;
                    }
                    o.nodes.insert(*x);
                }
                Some(false) => {
                    o.nodes.remove(x);
                }
                None => {}
            }
        }
        // a node that stops being driven gets its stop value once
        let stopped: Vec<i64> = was_driven.into_iter().filter(|x| !self.dev.outputs[oi].nodes.contains(x)).collect();
        if !stopped.is_empty() {
            let mut tmp = BTreeSet::new();
            std::mem::swap(&mut tmp, &mut self.dev.outputs[oi].nodes);
            self.dev.outputs[oi].nodes = stopped.iter().copied().collect();
            let frames = self.dev.stop_frames(Some(oi));
            self.dev.outputs[oi].nodes = tmp;
            self.sync_periodic();
            if let Some(l) = &self.link {
                for f in frames {
                    let _ = l.tx.send(Cmd::Send(f));
                }
            }
        } else {
            self.sync_periodic();
        }
    }

    pub(super) fn send_command(&mut self, ci: usize, n: i64) {
        let Some(p) = self.dev.profile.clone() else { return };
        let args = self.dev.args.get(ci).cloned().unwrap_or_default();
        let name = p.commands[ci].name.clone();
        match p.build_command(ci, n, &args) {
            Ok(f) => {
                if !self.is_open {
                    self.dev_log(Kind::Err, "Open the adapter on the CAN Terminal page first.".into());
                    return;
                }
                if let Some((ext, id, reply, nack)) = p.reply_match(ci, n) {
                    let deadline = Instant::now() + Duration::from_millis(p.commands[ci].timeout_ms);
                    self.dev.pending.retain(|q| !(q.id == id && q.ext == ext && q.reply == reply));
                    self.dev.pending.push(Pending { name: name.clone(), node: n, ext, id, reply, nack, deadline });
                }
                self.dev_log(Kind::Tx, format!("{name} → node {n}: {}", f.display()));
                if let Some(l) = &self.link {
                    let _ = l.tx.send(Cmd::Send(f));
                }
            }
            Err(e) => self.dev_log(Kind::Err, format!("{name}: {e}")),
        }
    }

    pub(super) fn run_sequence(&mut self, si: usize, n: i64, card: Option<String>) {
        let Some(p) = self.dev.profile.clone() else { return };
        let seq = &p.sequences[si];
        if self.dev.running.is_some() {
            self.dev_log(Kind::Warn, "Another sequence is running. Abort it first.".into());
            return;
        }
        let Some(link) = self.link.as_ref().filter(|_| self.is_open) else {
            self.dev_log(Kind::Err, "Open the adapter on the CAN Terminal page first.".into());
            return;
        };
        let abort = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let args = seq.args.iter().zip(self.dev.seq_args.get(si).cloned().unwrap_or_default()).map(|(a, v)| (a.0.clone(), v)).collect();
        let ctx = script::Ctx {
            name: seq.name.clone(),
            script: seq.script.clone(),
            profile: p.clone(),
            link: link.tx.clone(),
            shared: self.dev.shared.clone(),
            abort: abort.clone(),
            ev: tx,
            repaint: self.ctx.clone(),
            node: n,
            card_nodes: self.dev.card_node_list(),
            args,
        };
        self.dev_log(Kind::Note, format!("==== {} (node {n}) ====", seq.name));
        self.dev.running = Some(Running { name: seq.name.clone(), abort, rx, stop_after: seq.stop_after, card });
        script::spawn(ctx);
    }

    pub(super) fn dev_log(&mut self, kind: Kind, text: String) {
        let t = self.stamp(Instant::now());
        self.dev.log.push(LogLine { t, kind, text: text.clone() });
        if self.dev.log.len() > 2000 {
            self.dev.log.drain(..500);
        }
        if matches!(kind, Kind::Info | Kind::Err | Kind::Warn | Kind::Note) {
            self.note(kind, &text);
        }
    }

    /// Runs every frame, whatever page is shown: timeouts, node changes, script events.
    pub(super) fn poll_dev(&mut self) {
        for (k, t) in self.dev.tick() {
            self.dev_log(k, t);
        }
        let mut evs = Vec::new();
        if let Some(r) = &self.dev.running {
            while let Ok(e) = r.rx.try_recv() {
                evs.push(e);
            }
        }
        for e in evs {
            match e {
                ScriptEv::Log(k, t) => {
                    let kind = match k {
                        LogKind::Text => Kind::Text,
                        LogKind::Info => Kind::Info,
                        LogKind::Warn => Kind::Warn,
                        LogKind::Err | LogKind::Fail => Kind::Err,
                        LogKind::Pass => Kind::Rx,
                        LogKind::Prompt => Kind::Note,
                    };
                    self.dev_log(kind, t);
                }
                ScriptEv::Drive { output, node, values } => {
                    if output < self.dev.outputs.len() {
                        let drive = Some(values.is_some());
                        self.set_output(output, node, values, drive);
                    }
                }
                ScriptEv::StopAll => self.stop_outputs(None),
                ScriptEv::SetCardNode(n) => {
                    if let Some(c) = self.dev.running.as_ref().and_then(|r| r.card.clone()) {
                        self.dev.card_nodes.insert(c.clone(), n);
                        for p in &mut self.dev.popups {
                            if p.2.as_deref() == Some(c.as_str()) {
                                p.1 = n;
                            }
                        }
                    }
                }
                ScriptEv::Confirm(t, tx) => self.dev.confirm = Some(Confirm::Script(t, tx)),
                ScriptEv::Done { passed, failed, outcome } => {
                    let Some(r) = self.dev.running.take() else { continue };
                    let summary = if passed + failed > 0 { format!(": {passed} passed, {failed} failed") } else { String::new() };
                    match outcome {
                        Outcome::Finished => self.dev_log(if failed > 0 { Kind::Err } else { Kind::Note }, format!("==== {} finished{summary} ====", r.name)),
                        Outcome::Aborted => self.dev_log(Kind::Warn, format!("==== {} aborted{summary} ====", r.name)),
                        Outcome::Error(e) => self.dev_log(Kind::Err, format!("==== {} stopped: {e}{summary} ====", r.name)),
                    }
                    if matches!(self.dev.confirm, Some(Confirm::Script(..))) {
                        self.dev.confirm = None;
                    }
                    if r.stop_after || r.abort.load(Ordering::Relaxed) {
                        self.stop_outputs(None);
                    }
                    if let Some(from) = self.exit_after_seq {
                        // --sequence NAME --exit: report on stdout and quit
                        for l in &self.dev.log[from.min(self.dev.log.len())..] {
                            println!("{}  {}", l.t, l.text);
                        }
                        let ok = failed == 0 && !r.abort.load(Ordering::Relaxed) && self.dev.log.last().is_some_and(|l| l.text.contains("finished"));
                        std::thread::sleep(Duration::from_millis(200));
                        std::process::exit(if ok { 0 } else { 1 });
                    }
                }
            }
        }
    }
}

/// `--autopilot`: a short scripted session on the demo bus, used for the README screenshots.
#[derive(Default)]
pub struct Auto {
    opened: Option<Instant>,
    step: usize,
}

impl App {
    pub(super) fn autopilot(&mut self) {
        let Some(a) = &mut self.auto else { return };
        if !self.is_open {
            return;
        }
        let opened = *a.opened.get_or_insert_with(Instant::now);
        let t = opened.elapsed().as_secs_f32();
        let step = a.step;
        let p = self.dev.profile.clone();
        let seq = |name: &str| p.as_ref().and_then(|p| p.sequences.iter().position(|s| s.name == name));
        let cmd = |name: &str| p.as_ref().and_then(|p| p.commands.iter().position(|c| c.name == name));
        match step {
            0 if t > 0.3 => {
                if let Some(si) = seq("Start FOC both") {
                    self.run_sequence(si, 1, None);
                } else {
                    for n in [1, 2] {
                        if let Some(ci) = cmd("Start FOC") {
                            self.send_command(ci, n);
                        }
                    }
                }
            }
            1 if t > 2.2 => {
                for (n, v) in [(1, 1.2), (2, -0.6)] {
                    if !self.dev.outputs.is_empty() {
                        self.set_output(0, n, Some(vec![v]), Some(true));
                    }
                }
                for n in [1, 2] {
                    self.dev.plot.insert((Some(n), "velocity.velocity".to_string()));
                }
                self.dev.plot_secs = 6.0;
            }
            2 if t > 3.8 => {
                for (n, v) in [(1, 0.4), (2, -1.5)] {
                    if !self.dev.outputs.is_empty() {
                        self.set_output(0, n, Some(vec![v]), None);
                    }
                }
            }
            _ => return,
        }
        if let Some(a) = &mut self.auto {
            a.step += 1;
        }
    }
}
