//! "Device" page: everything driven by a device profile (profiles/*.toml) — decoded signals,
//! node table, plots, signal outputs (sliders sent periodically) and command buttons.

use super::{th, App, Kind, C_ERR, C_INFO, C_RX, C_TIME, C_TX, C_WARN};
use crate::can::CanFrame;
use crate::device::Cmd;
use crate::periodic::Job;
use crate::profile::Profile;
use eframe::egui::{self, RichText};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Tx tags at or above this belong to profile outputs (below: rows of the periodic list).
pub const OUTPUT_TAG: usize = 1 << 20;
const HISTORY_POINTS: usize = 20_000;
const HISTORY_SECS: f64 = 600.0;

type SigKey = (Option<i64>, String);

struct Sig {
    text: String,
    num: Option<f64>,
    alarm: bool,
    last: Instant,
    tx: bool,
    hist: VecDeque<[f64; 2]>,
}

struct Pending {
    name: String,
    node: i64,
    ext: bool,
    id: u32,
    reply: Vec<u8>,
    nack: Option<Vec<u8>>,
    deadline: Instant,
}

pub struct OutState {
    pub on: bool,
    /// nodes this output drives (per-node messages); ignored for messages without n
    pub nodes: BTreeSet<i64>,
    /// slider values per node (node 0 for messages without n)
    pub values: BTreeMap<i64, Vec<f64>>,
    pub sent: u64,
}

pub struct DevState {
    pub profile: Option<Profile>,
    pub path: Option<PathBuf>,
    pub load_err: Option<String>,
    pub decode_trace: bool,
    signals: BTreeMap<SigKey, Sig>,
    heard: BTreeMap<i64, Instant>,
    sel_node: i64,
    args: Vec<Vec<f64>>,
    pending: Vec<Pending>,
    log: Vec<(String, Kind, String)>,
    pub outputs: Vec<OutState>,
    plot: BTreeSet<SigKey>,
    plot_secs: f64,
    follow: bool,
    t0: Instant,
    confirm: Option<(usize, i64)>,
    filter: String,
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
            sel_node: 1,
            args: Vec::new(),
            pending: Vec::new(),
            log: Vec::new(),
            outputs: Vec::new(),
            plot: BTreeSet::new(),
            plot_secs: 10.0,
            follow: true,
            t0: Instant::now(),
            confirm: None,
            filter: String::new(),
        };
        if let Ok(p) = std::fs::read_to_string(profile_setting()) {
            let p = PathBuf::from(p.trim());
            if p.is_file() {
                s.load(p);
            }
        }
        s
    }

    /// Loads (or reloads) a profile. Keeps slider values and plots when the names still match.
    pub fn load(&mut self, path: PathBuf) {
        match Profile::load(&path) {
            Ok(p) => {
                let keep_values = self.profile.as_ref().map(|old| old.outputs.iter().map(|o| o.name.clone()).collect::<Vec<_>>());
                self.args = p.commands.iter().map(|c| c.args.iter().map(|a| a.1).collect()).collect();
                let old_outputs = std::mem::take(&mut self.outputs);
                self.outputs = p
                    .outputs
                    .iter()
                    .map(|o| {
                        let prev = keep_values.as_ref().and_then(|names| names.iter().position(|n| n == &o.name)).and_then(|i| old_outputs.get(i));
                        OutState {
                            on: false,
                            nodes: prev.map(|x| x.nodes.clone()).unwrap_or_default(),
                            values: prev.map(|x| x.values.clone()).unwrap_or_default(),
                            sent: 0,
                        }
                    })
                    .collect();
                if let Some(n) = &p.nodes {
                    self.sel_node = self.sel_node.clamp(n.min, n.max);
                }
                self.pending.clear();
                self.load_err = None;
                self.profile = Some(p);
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
        let _ = std::fs::remove_file(profile_setting());
    }

    /// Decodes a frame seen on the bus (`rx`) or sent by us. Updates signals, nodes and command
    /// replies; returns the text for the trace (empty when nothing matched) and result lines.
    pub fn on_frame(&mut self, f: &CanFrame, at: Instant, rx: bool) -> (String, Vec<(Kind, String)>) {
        let Some(p) = &self.profile else { return (String::new(), Vec::new()) };
        let decoded = p.decode(f);
        let mut results = Vec::new();
        if rx {
            // command replies
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
        for d in &decoded {
            texts.push(p.describe(d));
            if rx {
                if let (Some(n), Some(nodes)) = (d.node, &p.nodes) {
                    let is_hb = match nodes.heartbeat {
                        None => true,
                        Some((m, v)) => m == d.message && (v.is_none() || v == d.variant),
                    };
                    if is_hb {
                        self.heard.insert(n, at);
                    }
                }
            }
            for v in &d.values {
                let s = self.signals.entry((d.node, v.key.clone())).or_insert_with(|| Sig { text: String::new(), num: None, alarm: false, last: at, tx: !rx, hist: VecDeque::new() });
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
        (texts.join(" | "), results)
    }

    /// Command replies that did not come in time.
    pub fn timeouts(&mut self) -> Vec<(Kind, String)> {
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
        out
    }

    fn values_for(&self, oi: usize, n: i64) -> Vec<f64> {
        let p = self.profile.as_ref().unwrap();
        let m = &p.messages[p.outputs[oi].message];
        self.outputs[oi].values.get(&n).cloned().unwrap_or_else(|| m.signals.iter().map(|s| s.stop.or(s.default).filter(|v| !v.is_nan()).unwrap_or(0.0)).collect())
    }

    fn driven(&self, oi: usize) -> Vec<i64> {
        let p = self.profile.as_ref().unwrap();
        if p.messages[p.outputs[oi].message].per_node {
            self.outputs[oi].nodes.iter().copied().collect()
        } else {
            vec![0]
        }
    }

    /// Periodic jobs for every output that is on.
    pub fn jobs(&mut self) -> (Vec<Job>, Vec<String>) {
        let mut jobs = Vec::new();
        let mut errs = Vec::new();
        let Some(p) = &self.profile else { return (jobs, errs) };
        for (oi, o) in p.outputs.iter().enumerate() {
            if !self.outputs[oi].on {
                continue;
            }
            let driven = self.driven(oi);
            let vals: BTreeMap<i64, Vec<f64>> = driven.iter().map(|&n| (n, self.values_for(oi, n))).collect();
            match p.encode_output(oi, &|n| vals.get(&n).cloned()) {
                Ok(frames) => {
                    for frame in frames {
                        jobs.push(Job { tag: OUTPUT_TAG + oi, frame, period: Duration::from_millis(o.period_ms) });
                    }
                }
                Err(e) => {
                    errs.push(format!("{}: {e}", o.name));
                    self.outputs[oi].on = false;
                }
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
            if !self.outputs[oi].on || only.is_some_and(|x| x != oi) {
                continue;
            }
            let m = &p.messages[o.message];
            if m.signals.iter().all(|s| s.stop.is_none()) {
                continue;
            }
            let stop: Vec<f64> = m.signals.iter().map(|s| s.stop.or(s.default).unwrap_or(0.0)).collect();
            let driven = self.driven(oi);
            if let Ok(f) = p.encode_output(oi, &|n| if driven.contains(&n) { Some(stop.clone()) } else { None }) {
                out.extend(f);
            }
        }
        out
    }

    pub fn any_output_on(&self) -> bool {
        self.outputs.iter().any(|o| o.on)
    }

    pub fn reset_seen(&mut self) {
        self.signals.clear();
        self.heard.clear();
        self.t0 = Instant::now();
    }
}

enum Action {
    Load(PathBuf),
    Reload,
    Unload,
    Command(usize, i64),
    OutputChanged,
    StopOutput(usize),
}

impl App {
    /// Sends the stop frames of all outputs that are on (or one), then switches them off.
    pub(super) fn stop_outputs(&mut self, only: Option<usize>) {
        let frames = self.dev.stop_frames(only);
        for (i, o) in self.dev.outputs.iter_mut().enumerate() {
            if only.is_none_or(|x| x == i) {
                o.on = false;
            }
        }
        self.sync_periodic();
        if let Some(l) = &self.link {
            for f in frames {
                let _ = l.tx.send(Cmd::Send(f));
            }
        }
    }

    fn send_command(&mut self, ci: usize, n: i64) {
        let Some(p) = &self.dev.profile else { return };
        let args = self.dev.args.get(ci).cloned().unwrap_or_default();
        let name = p.commands[ci].name.clone();
        match p.build_command(ci, n, &args) {
            Ok(f) => {
                if !self.is_open {
                    self.note(Kind::Err, "Open the adapter on the CAN Terminal page first.");
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

    pub(super) fn dev_log(&mut self, kind: Kind, text: String) {
        let t = self.stamp(Instant::now());
        self.dev.log.push((t, kind, text.clone()));
        if self.dev.log.len() > 500 {
            self.dev.log.drain(..100);
        }
        if matches!(kind, Kind::Info | Kind::Err | Kind::Warn) {
            self.note(kind, &text);
        }
    }

    pub(super) fn device_ui(&mut self, ui: &mut egui::Ui) {
        for (k, t) in self.dev.timeouts() {
            self.dev_log(k, t);
        }
        if !self.dev.pending.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        let mut actions = Vec::new();

        egui::Panel::top("dev_profile").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).fill(th((236, 240, 247), (30, 33, 40)))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Device profile").strong().color(th((40, 70, 140), (130, 170, 240))));
                let name = self.dev.profile.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "none loaded".into());
                ui.label(RichText::new(name).strong());
                if ui.button("📂 Load…").on_hover_text("Open a .toml device profile").clicked() {
                    let mut d = rfd::FileDialog::new().add_filter("Device profile", &["toml"]);
                    if let Some(dir) = self.dev.path.as_ref().and_then(|p| p.parent()) {
                        d = d.set_directory(dir);
                    }
                    if let Some(p) = d.pick_file() {
                        actions.push(Action::Load(p));
                    }
                }
                if let Some(p) = &self.dev.path {
                    let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(RichText::new(file).small().color(C_TIME())).on_hover_text(p.display().to_string());
                    if ui.button("⟳ Reload").on_hover_text("Read the file again after editing it").clicked() {
                        actions.push(Action::Reload);
                    }
                    if ui.button("✖").on_hover_text("Unload the profile").clicked() {
                        actions.push(Action::Unload);
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.dev.decode_trace, "Decode in trace").on_hover_text("Add the decoded values to frames on the CAN Terminal page");
                if ui.button("Clear values").clicked() {
                    self.dev.reset_seen();
                }
                ui.separator();
                let (txt, col) = if self.is_open { ("Adapter open", C_RX()) } else { ("Adapter closed – open it on the CAN Terminal page", C_WARN()) };
                ui.label(RichText::new(txt).color(col));
            });
            if let Some(e) = &self.dev.load_err {
                ui.label(RichText::new(format!("Cannot load the profile: {e}")).color(C_ERR()));
            }
        });

        if self.dev.profile.is_none() {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(20.0);
                ui.label(RichText::new("Load a device profile to decode your own protocol.").size(16.0));
                ui.label(
                    "A profile is a .toml file that names your CAN messages and signals, the node numbering, \
                     the commands your boards understand and the values you want to send with sliders. \
                     See profiles/README.md in the repository; profiles/bldc-driver.toml is a complete example.",
                );
            });
            self.apply(actions);
            return;
        }

        egui::Panel::left("dev_left").resizable(true).default_size(600.0).min_size(380.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.nodes_ui(ui);
                self.outputs_ui(ui, &mut actions);
                self.commands_ui(ui, &mut actions);
            });
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.signals_ui(ui);
        });

        if let Some((ci, n)) = self.dev.confirm {
            let name = self.dev.profile.as_ref().and_then(|p| p.commands.get(ci)).map(|c| (c.name.clone(), c.help.clone()));
            if let Some((name, help)) = name {
                let mut open = true;
                egui::Window::new("Confirm").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).open(&mut open).show(ui.ctx(), |ui| {
                    ui.label(RichText::new(format!("Send \"{name}\" to node {n}?")).strong());
                    if !help.is_empty() {
                        ui.label(help);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Send").clicked() {
                            actions.push(Action::Command(ci, n));
                            self.dev.confirm = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.dev.confirm = None;
                        }
                    });
                });
                if !open {
                    self.dev.confirm = None;
                }
            } else {
                self.dev.confirm = None;
            }
        }
        self.apply(actions);
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Load(p) => {
                    self.stop_outputs(None);
                    self.dev.load(p);
                    self.after_load();
                }
                Action::Reload => {
                    if let Some(p) = self.dev.path.clone() {
                        self.stop_outputs(None);
                        self.dev.load(p);
                        self.after_load();
                    }
                }
                Action::Unload => {
                    self.stop_outputs(None);
                    self.dev.unload();
                }
                Action::Command(ci, n) => self.send_command(ci, n),
                Action::OutputChanged => self.sync_periodic(),
                Action::StopOutput(i) => self.stop_outputs(Some(i)),
            }
        }
    }

    fn after_load(&mut self) {
        match (&self.dev.profile, &self.dev.load_err) {
            (Some(p), _) => {
                let msg = format!("Profile loaded: {} ({} messages, {} commands, {} outputs)", p.name, p.messages.len(), p.commands.len(), p.outputs.len());
                self.note(Kind::Note, &msg);
            }
            (None, Some(e)) => {
                let e = e.clone();
                self.note(Kind::Err, &format!("Profile: {e}"));
            }
            _ => {}
        }
    }

    fn section(ui: &mut egui::Ui, title: &str) {
        ui.add_space(6.0);
        ui.label(RichText::new(title).strong().size(15.0).color(th((40, 70, 140), (130, 170, 240))));
        ui.separator();
    }

    fn nodes_ui(&mut self, ui: &mut egui::Ui) {
        let p = self.dev.profile.as_ref().unwrap();
        let Some(nodes) = &p.nodes else { return };
        Self::section(ui, &format!("{}s", nodes.label));
        if self.dev.heard.is_empty() {
            ui.label(RichText::new(if self.is_open { "No node heard yet." } else { "Open the adapter to see the nodes on the bus." }).color(C_TIME()));
            return;
        }
        let timeout = Duration::from_millis(nodes.timeout_ms);
        let cols: Vec<String> = nodes.columns.clone();
        let label = nodes.label.clone();
        let mut select = None;
        egui::ScrollArea::horizontal().id_salt("nodes_h").show(ui, |ui| {
            egui::Grid::new("nodes").striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new(&label).strong());
                ui.label(RichText::new("Seen").strong());
                for c in &cols {
                    ui.label(RichText::new(c.rsplit('.').next().unwrap_or(c)).strong()).on_hover_text(c);
                }
                ui.end_row();
                for (&n, &last) in &self.dev.heard {
                    let age = last.elapsed();
                    let alive = age < timeout;
                    let sel = self.dev.sel_node == n;
                    if ui.selectable_label(sel, RichText::new(format!("{n}")).strong()).on_hover_text("Select for commands").clicked() {
                        select = Some(n);
                    }
                    let (t, col) = if alive { ("● alive".to_string(), C_RX()) } else { (format!("● missing {:.1} s", age.as_secs_f32()), C_ERR()) };
                    ui.label(RichText::new(t).color(col));
                    for c in &cols {
                        match self.dev.signals.get(&(Some(n), c.clone())) {
                            Some(s) => ui.label(RichText::new(&s.text).monospace().color(if s.alarm { C_ERR() } else if alive { th((20, 20, 20), (225, 225, 225)) } else { C_TIME() })),
                            None => ui.label(RichText::new("–").color(C_TIME())),
                        };
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(n) = select {
            self.dev.sel_node = n;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    fn outputs_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = self.dev.profile.as_ref().unwrap();
        if p.outputs.is_empty() {
            return;
        }
        Self::section(ui, "Outputs");
        ui.label(RichText::new("Sent every period while On. Off or Esc sends each signal's stop value once.").small().color(C_TIME()));
        let range = p.node_range();
        let heard: BTreeSet<i64> = self.dev.heard.keys().copied().collect();
        let sel_node = self.dev.sel_node;
        let mut changed = false;
        for oi in 0..p.outputs.len() {
            let o = &p.outputs[oi];
            let m = &p.messages[o.message];
            let st = &mut self.dev.outputs[oi];
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let was = st.on;
                let on_text = if st.on { "⏹ On" } else { "▶ Off" };
                let b = egui::Button::new(RichText::new(on_text).strong()).fill(if st.on { th((200, 235, 205), (40, 90, 50)) } else { th((225, 228, 235), (55, 58, 66)) });
                if ui.add_enabled(self.is_open || st.on, b).on_hover_text(if self.is_open { "Start / stop sending" } else { "Open the adapter first" }).clicked() {
                    if was {
                        actions.push(Action::StopOutput(oi));
                    } else {
                        st.on = true;
                        st.sent = 0;
                        changed = true;
                    }
                }
                ui.label(RichText::new(&o.name).strong());
                ui.label(RichText::new(format!("every {} ms", o.period_ms)).color(C_TIME()));
                ui.label(RichText::new(format!("sent {}", st.sent)).monospace().color(C_TX()));
                if ui.button("Zero").on_hover_text("Set every slider to its stop value (or 0)").clicked() {
                    st.values.clear();
                    changed = true;
                }
            });
            let show_nodes: Vec<i64> = if m.per_node {
                let mut v: BTreeSet<i64> = heard.iter().copied().chain(st.nodes.iter().copied()).collect();
                v.insert(sel_node);
                v.into_iter().filter(|n| range.contains(n)).collect()
            } else {
                vec![0]
            };
            ui.spacing_mut().slider_width = 260.0;
            egui::Grid::new(("out", oi)).spacing([10.0, 4.0]).show(ui, |ui| {
                for n in show_nodes {
                    if m.per_node {
                        let mut drive = st.nodes.contains(&n);
                        if ui.checkbox(&mut drive, format!("node {n}")).on_hover_text("Drive this node").changed() {
                            if drive {
                                st.nodes.insert(n);
                            } else {
                                st.nodes.remove(&n);
                            }
                            changed = true;
                        }
                    } else {
                        ui.label("");
                    }
                    let mut vals = st.values.get(&n).cloned().unwrap_or_else(|| m.signals.iter().map(|s| s.stop.or(s.default).filter(|v| !v.is_nan()).unwrap_or(0.0)).collect());
                    let mut vchanged = false;
                    for (s, v) in m.signals.iter().zip(vals.iter_mut()) {
                        let (lo, hi) = (s.min.unwrap_or(-100.0), s.max.unwrap_or(100.0));
                        let r = ui.add(egui::Slider::new(v, lo..=hi).text(format!("{} {}", s.name, s.unit)).fixed_decimals(s.decimals().min(4)).clamping(egui::SliderClamping::Always));
                        vchanged |= r.changed();
                    }
                    if vchanged {
                        st.values.insert(n, vals);
                        changed = true;
                    }
                    ui.end_row();
                }
            });
        }
        if changed {
            actions.push(Action::OutputChanged);
        }
    }

    fn commands_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = self.dev.profile.as_ref().unwrap();
        if p.commands.is_empty() {
            return;
        }
        Self::section(ui, "Commands");
        if let Some(nodes) = &p.nodes {
            ui.horizontal(|ui| {
                ui.label(format!("Send to {}:", nodes.label.to_lowercase()));
                let heard = &self.dev.heard;
                egui::ComboBox::from_id_salt("cmd_node").selected_text(self.dev.sel_node.to_string()).show_ui(ui, |ui| {
                    for n in nodes.min..=nodes.max {
                        let t = if heard.contains_key(&n) { format!("{n}  ●") } else { n.to_string() };
                        ui.selectable_value(&mut self.dev.sel_node, n, t);
                    }
                });
            });
        }
        let n = self.dev.sel_node;
        egui::Grid::new("commands").spacing([10.0, 4.0]).show(ui, |ui| {
            for (ci, c) in p.commands.iter().enumerate() {
                let mut b = ui.add_enabled(self.is_open, egui::Button::new(&c.name).min_size(egui::vec2(150.0, 0.0)));
                if !c.help.is_empty() {
                    b = b.on_hover_text(&c.help);
                }
                if b.clicked() {
                    if c.confirm {
                        self.dev.confirm = Some((ci, n));
                    } else {
                        actions.push(Action::Command(ci, n));
                    }
                }
                ui.horizontal(|ui| {
                    for (ai, a) in c.args.iter().enumerate() {
                        ui.label(&a.0);
                        let v = &mut self.dev.args[ci][ai];
                        let whole = [Some(a.1), a.2, a.3].iter().flatten().all(|x| x.fract() == 0.0);
                        let mut d = egui::DragValue::new(v).speed(if whole { 0.2 } else { 0.1 });
                        if whole {
                            d = d.fixed_decimals(0);
                        }
                        if let (Some(lo), Some(hi)) = (a.2, a.3) {
                            d = d.range(lo..=hi);
                        }
                        ui.add(d);
                    }
                    if c.confirm {
                        ui.label(RichText::new("asks first").small().color(C_TIME()));
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Results").strong());
        egui::Frame::new().fill(th((255, 255, 255), (20, 22, 27))).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("cmdlog").max_height(160.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                if self.dev.log.is_empty() {
                    ui.label(RichText::new("Command replies appear here.").color(C_TIME()));
                }
                for (t, k, text) in &self.dev.log {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(t).monospace().color(C_TIME()));
                        ui.label(RichText::new(text).monospace().color(k.color()));
                    });
                }
            });
        });
    }

    fn signals_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Signals").strong().size(15.0).color(th((40, 70, 140), (130, 170, 240))));
            ui.add(egui::TextEdit::singleline(&mut self.dev.filter).desired_width(160.0).hint_text("filter"));
            ui.label(RichText::new("Tick a value to plot it.").color(C_TIME()));
        });
        let filter = self.dev.filter.to_lowercase();
        let avail = ui.available_height();
        let mut toggles = Vec::new();
        egui::ScrollArea::vertical().id_salt("sigs").max_height((avail * 0.42).max(120.0)).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("sigs_grid").striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                for h in ["", "Node", "Signal", "Value", "Age"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (key, s) in &self.dev.signals {
                    if !filter.is_empty() && !key.1.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let mut on = self.dev.plot.contains(key);
                    if ui.add_enabled(s.num.is_some() || !s.hist.is_empty(), egui::Checkbox::without_text(&mut on)).changed() {
                        toggles.push((key.clone(), on));
                    }
                    ui.label(key.0.map(|n| n.to_string()).unwrap_or_else(|| "–".into()));
                    let name = RichText::new(&key.1).monospace();
                    ui.label(if s.tx { name.color(C_TX()) } else { name });
                    ui.label(RichText::new(&s.text).monospace().color(if s.alarm { C_ERR() } else if s.tx { C_TX() } else { C_INFO() }));
                    ui.label(RichText::new(format!("{:.1} s", s.last.elapsed().as_secs_f32())).small().color(C_TIME()));
                    ui.end_row();
                }
            });
            if self.dev.signals.is_empty() {
                ui.label(RichText::new("No decoded frames yet.").color(C_TIME()));
            }
        });
        for (k, on) in toggles {
            if on {
                self.dev.plot.insert(k);
            } else {
                self.dev.plot.remove(&k);
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Plot").strong());
            ui.checkbox(&mut self.dev.follow, "Follow").on_hover_text("Off: drag and zoom freely");
            ui.add_enabled(self.dev.follow, egui::DragValue::new(&mut self.dev.plot_secs).range(1.0..=HISTORY_SECS).suffix(" s").speed(0.5)).on_hover_text("Time window");
            if ui.button("Clear plot").clicked() {
                self.dev.plot.clear();
            }
            if ui.button("💾 CSV").on_hover_text("Save the plotted signals' history").clicked() {
                self.save_csv();
            }
        });
        let now = self.dev.t0.elapsed().as_secs_f64();
        let follow = self.dev.follow;
        let secs = self.dev.plot_secs;
        let plot = egui_plot::Plot::new("dev_plot")
            .legend(egui_plot::Legend::default())
            .x_axis_label("s")
            .allow_drag(!follow)
            .allow_zoom(!follow)
            .allow_scroll(!follow);
        plot.show(ui, |pui| {
            for key in &self.dev.plot {
                if let Some(s) = self.dev.signals.get(key) {
                    let name = match key.0 {
                        Some(n) => format!("{} n{n}", key.1),
                        None => key.1.clone(),
                    };
                    let pts: Vec<[f64; 2]> = s.hist.iter().filter(|p| !follow || p[0] >= now - secs - 1.0).copied().collect();
                    pui.line(egui_plot::Line::new(name, pts));
                }
            }
            if follow {
                pui.set_auto_bounds([false, true]);
                pui.set_plot_bounds_x(now - secs..=now);
            }
        });
        if follow && !self.dev.plot.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }

    fn save_csv(&mut self) {
        if self.dev.plot.is_empty() {
            self.note(Kind::Warn, "Tick some signals to plot first.");
            return;
        }
        let Some(path) = rfd::FileDialog::new().set_file_name("signals.csv").save_file() else { return };
        let mut out = String::from("time_s,node,signal,value\n");
        for key in &self.dev.plot {
            if let Some(s) = self.dev.signals.get(key) {
                for p in &s.hist {
                    out += &format!("{:.6},{},{},{}\n", p[0], key.0.map(|n| n.to_string()).unwrap_or_default(), key.1, p[1]);
                }
            }
        }
        if let Err(e) = std::fs::write(&path, out) {
            self.note(Kind::Err, &format!("Cannot save: {e}"));
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
        let cmd = |app: &App, name: &str| app.dev.profile.as_ref().and_then(|p| p.commands.iter().position(|c| c.name == name));
        match step {
            0 if t > 0.3 => {
                for (name, n) in [("Get info", 1), ("Get info", 2), ("Start FOC", 1), ("Start FOC", 2)] {
                    if let Some(ci) = cmd(self, name) {
                        self.send_command(ci, n);
                    }
                }
            }
            1 if t > 1.8 => {
                if let Some(o) = self.dev.outputs.first_mut() {
                    o.nodes = [1, 2].into_iter().collect();
                    o.values.insert(1, vec![1.2]);
                    o.values.insert(2, vec![-0.6]);
                    o.on = true;
                }
                self.dev.sel_node = 1;
                for k in [(Some(1), "velocity.velocity"), (Some(2), "velocity.velocity")] {
                    self.dev.plot.insert((k.0, k.1.to_string()));
                }
                self.dev.plot_secs = 6.0;
                self.sync_periodic();
                if let Some(ci) = cmd(self, "Set velocity interval") {
                    self.dev.args[ci][0] = 20.0;
                    self.send_command(ci, 2);
                }
            }
            2 if t > 3.6 => {
                if let Some(o) = self.dev.outputs.first_mut() {
                    o.values.insert(1, vec![0.4]);
                    o.values.insert(2, vec![-1.5]);
                }
                self.sync_periodic();
            }
            _ => return,
        }
        if let Some(a) = &mut self.auto {
            a.step += 1;
        }
    }
}
