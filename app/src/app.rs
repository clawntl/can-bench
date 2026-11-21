//! The window: CAN terminal + firmware updater.

use crate::can::{self, CanFrame};
use crate::device::{self, AdapterRef, BusMode, Cmd, Ev, FwEv, FwTask, Link, OpenConfig};
use crate::dfufile;
use eframe::egui::{self, Color32, RichText};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const NOMINAL: [u32; 14] = [5_000, 10_000, 20_000, 33_333, 50_000, 62_500, 83_333, 100_000, 125_000, 250_000, 500_000, 800_000, 1_000_000, 0];
const DATA: [u32; 9] = [1_000_000, 2_000_000, 4_000_000, 5_000_000, 8_000_000, 10_000_000, 500_000, 3_000_000, 0];



static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn is_dark() -> bool {
    DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Picks the light or the dark variant of a colour.
fn th(l: (u8, u8, u8), d: (u8, u8, u8)) -> Color32 {
    let (r, g, b) = if is_dark() { d } else { l };
    Color32::from_rgb(r, g, b)
}
#[allow(non_snake_case)]
fn C_TX() -> Color32 {
    th((20, 40, 220), (120, 160, 255))
}
#[allow(non_snake_case)]
fn C_RX() -> Color32 {
    th((0, 135, 30), (90, 210, 110))
}
#[allow(non_snake_case)]
fn C_ECHO() -> Color32 {
    th((128, 128, 0), (205, 200, 90))
}
#[allow(non_snake_case)]
fn C_INFO() -> Color32 {
    th((180, 0, 180), (230, 125, 230))
}
#[allow(non_snake_case)]
fn C_TEXT() -> Color32 {
    th((30, 30, 30), (220, 222, 228))
}
#[allow(non_snake_case)]
fn C_WARN() -> Color32 {
    th((205, 110, 0), (245, 165, 70))
}
#[allow(non_snake_case)]
fn C_ERR() -> Color32 {
    th((210, 0, 0), (255, 100, 100))
}
#[allow(non_snake_case)]
fn C_TIME() -> Color32 {
    th((120, 130, 150), (130, 140, 160))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Tx,
    Rx,
    Echo,
    Info,
    Text,
    Warn,
    Err,
    Note,
}

impl Kind {
    fn color(self) -> Color32 {
        match self {
            Kind::Tx => C_TX(),
            Kind::Rx => C_RX(),
            Kind::Echo => C_ECHO(),
            Kind::Info => C_INFO(),
            Kind::Text => C_TEXT(),
            Kind::Warn => C_WARN(),
            Kind::Err => C_ERR(),
            Kind::Note => C_TEXT(),
        }
    }
}

struct Line {
    t: String,
    kind: Kind,
    text: String,
}

struct IdStat {
    frame: CanFrame,
    count: u64,
    last: Instant,
    period_ms: f64,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Terminal,
    Firmware,
}

struct FwFile {
    path: PathBuf,
    name: String,
    kind: String,
    board: String,
    mcu: String,
    size: usize,
    file: Option<dfufile::DfuFile>,
    err: Option<String>,
}

pub struct App {
    tab: Tab,
    adapters: Vec<AdapterRef>,
    sel: usize,
    // bus settings
    nom_baud: u32,
    nom_custom: u32,
    nom_sp: f64,
    fd: bool,
    data_baud: u32,
    data_custom: u32,
    data_sp: f64,
    mode: BusMode,
    auto_retx: bool,
    echo: bool,
    termination: bool,
    busload_on: bool,
    filters: String,
    suppress: bool,
    pad8: bool,
    // connection
    link: Option<Link>,
    is_open: bool,
    link_fd: bool,
    link_desc: String,
    identify: bool,
    // trace
    trace: Vec<Line>,
    autoscroll: bool,
    paused: bool,
    seen: HashSet<CanFrame>,
    table: BTreeMap<(bool, u32), IdStat>,
    view_table: bool,
    comment: String,
    logfile: Option<(std::io::BufWriter<std::fs::File>, PathBuf)>,
    base_inst: Instant,
    base_wall: chrono::DateTime<chrono::Local>,
    // send
    tx_id: String,
    tx_data: String,
    tx_ext: bool,
    tx_fd: bool,
    tx_brs: bool,
    tx_rtr: bool,
    repeat: bool,
    repeat_ms: u32,
    last_repeat: Instant,
    raw_cmd: String,
    // stats
    rx_count: u64,
    tx_count: u64,
    echo_count: u64,
    busload: Option<(u8, Instant)>,
    bus_state: (String, u8),
    // firmware
    fw_dirs: Vec<PathBuf>,
    own_fw_dir: PathBuf,
    fw_files: Vec<FwFile>,
    fw_sel: Option<usize>,
    fw_log: Vec<Line>,
    fw_rx: Option<Receiver<FwEv>>,
    fw_progress: Option<(f32, String)>,
    fw_leave: bool,
    fw_board: Option<(String, String, String)>,
    fw_identify: bool,
    // screenshot
    shot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
    shot_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn own_firmware_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("CanBench").join("Firmware")
}

fn theme_file() -> PathBuf {
    own_firmware_dir().parent().map(|p| p.join("theme.txt")).unwrap_or_else(|| PathBuf::from("theme.txt"))
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, std::sync::atomic::Ordering::Relaxed);
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>) -> Self {
        let dark = std::env::args().any(|a| a == "--dark")
            || std::fs::read_to_string(theme_file()).map(|s| s.trim() == "dark").unwrap_or(false);
        apply_theme(&cc.egui_ctx, dark);
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        cc.egui_ctx.set_global_style(style);

        let own = own_firmware_dir();
        let mut dirs = vec![own.clone()];
        let hud = PathBuf::from(r"C:\Program Files (x86)\HUD ECU Hacker\Driver\CANable Firmware Update\Firmware");
        if hud.is_dir() {
            dirs.push(hud);
        }
        let mut app = App {
            tab: Tab::Terminal,
            adapters: Vec::new(),
            sel: 0,
            nom_baud: 500_000,
            nom_custom: 500_000,
            nom_sp: 87.5,
            fd: false,
            data_baud: 2_000_000,
            data_custom: 2_000_000,
            data_sp: 75.0,
            mode: BusMode::Normal,
            auto_retx: true,
            echo: true,
            termination: false,
            busload_on: true,
            filters: String::new(),
            suppress: false,
            pad8: false,
            link: None,
            is_open: false,
            link_fd: false,
            link_desc: String::new(),
            identify: false,
            trace: Vec::new(),
            autoscroll: true,
            paused: false,
            seen: HashSet::new(),
            table: BTreeMap::new(),
            view_table: false,
            comment: String::new(),
            logfile: None,
            base_inst: Instant::now(),
            base_wall: chrono::Local::now(),
            tx_id: "123".into(),
            tx_data: "11 22 33 44 55 66 77 88".into(),
            tx_ext: false,
            tx_fd: false,
            tx_brs: false,
            tx_rtr: false,
            repeat: false,
            repeat_ms: 100,
            last_repeat: Instant::now(),
            raw_cmd: String::new(),
            rx_count: 0,
            tx_count: 0,
            echo_count: 0,
            busload: None,
            bus_state: ("Closed".into(), 0),
            fw_dirs: dirs,
            own_fw_dir: own,
            fw_files: Vec::new(),
            fw_sel: None,
            fw_log: Vec::new(),
            fw_rx: None,
            fw_progress: None,
            fw_leave: true,
            fw_board: None,
            fw_identify: false,
            shot,
            frames: 0,
            shot_requested: false,
            shot_done: Default::default(),
        };
        app.note(Kind::Tx, "Blue:   packets sent to the CAN bus");
        app.note(Kind::Rx, "Green:  packets received from the CAN bus");
        app.note(Kind::Echo, "Olive:  echo of sent packets that were acknowledged");
        app.note(Kind::Info, "Purple: adapter information");
        app.refresh();
        app.scan_firmware();
        if std::env::args().any(|a| a == "--firmware") {
            app.tab = Tab::Firmware;
        }
        app
    }

    fn stamp(&self, at: Instant) -> String {
        let d = at.saturating_duration_since(self.base_inst);
        let w = self.base_wall + chrono::Duration::from_std(d).unwrap_or_default();
        w.format("%H:%M:%S%.3f").to_string()
    }

    fn push(&mut self, at: Instant, kind: Kind, text: String) {
        let t = self.stamp(at);
        if let Some((w, _)) = &mut self.logfile {
            let _ = writeln!(w, "{t}  {text}");
        }
        self.trace.push(Line { t, kind, text });
        if self.trace.len() > 50_000 {
            self.trace.drain(..10_000);
        }
    }

    fn note(&mut self, kind: Kind, text: &str) {
        self.push(Instant::now(), kind, text.to_string());
    }

    fn refresh(&mut self) {
        let prev = self.adapters.get(self.sel).map(|a| a.label().to_string());
        self.adapters = device::scan();
        self.sel = prev.and_then(|p| self.adapters.iter().position(|a| a.label() == p)).unwrap_or(0);
        if self.sel >= self.adapters.len() {
            self.sel = 0;
        }
    }

    fn scan_firmware(&mut self) {
        self.fw_files.clear();
        for dir in self.fw_dirs.clone() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if ext != "dfu" && ext != "bin" {
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                if self.fw_files.iter().any(|f| f.name == name) {
                    continue;
                }
                let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                let parts: Vec<&str> = stem.split(" - ").map(str::trim).collect();
                let (mut kind, mut board, mut mcu) = (String::new(), String::new(), String::new());
                for part in &parts {
                    if part.starts_with("STM32") {
                        mcu = part.to_string();
                    } else if part.starts_with("Candlelight") || part.starts_with("Slcan") {
                        kind = part.to_string();
                    } else {
                        board = part.to_string();
                    }
                }
                let (file, err, size) = match std::fs::read(&p) {
                    Ok(bytes) => {
                        if ext == "bin" {
                            let n = bytes.len();
                            (Some(dfufile::from_bin(bytes, crate::dfu::FLASH_START)), None, n)
                        } else {
                            match dfufile::parse(&bytes) {
                                Ok(f) => {
                                    let n = f.total_bytes();
                                    (Some(f), None, n)
                                }
                                Err(e) => (None, Some(e), 0),
                            }
                        }
                    }
                    Err(e) => (None, Some(e.to_string()), 0),
                };
                self.fw_files.push(FwFile { path: p, name, kind, board, mcu, size, file, err });
            }
        }
        if self.fw_sel.is_some_and(|s| s >= self.fw_files.len()) {
            self.fw_sel = None;
        }
    }

    fn selected(&self) -> Option<AdapterRef> {
        self.adapters.get(self.sel).cloned()
    }

    fn baud(&self, data: bool) -> u32 {
        if data {
            if self.data_baud == 0 { self.data_custom } else { self.data_baud }
        } else if self.nom_baud == 0 {
            self.nom_custom
        } else {
            self.nom_baud
        }
    }

    fn open(&mut self, ctx: &egui::Context) {
        let Some(adapter) = self.selected() else {
            self.note(Kind::Err, "No adapter selected. Connect a CANable and click Refresh.");
            return;
        };
        let filters = match can::parse_filters(&self.filters) {
            Ok(f) => f,
            Err(e) => {
                self.note(Kind::Err, &e);
                return;
            }
        };
        let cfg = OpenConfig {
            adapter: adapter.clone(),
            nominal: (self.baud(false), self.nom_sp),
            data: if self.fd { Some((self.baud(true), self.data_sp)) } else { None },
            mode: self.mode,
            auto_retransmit: self.auto_retx,
            echo: self.echo,
            termination: self.termination,
            busload: self.busload_on,
            filters,
        };
        self.note(Kind::Note, &format!("Opening {}", adapter.label()));
        self.seen.clear();
        let c = ctx.clone();
        self.link = Some(device::open(cfg, move || c.request_repaint()));
        self.bus_state = ("Opening…".into(), 0);
    }

    fn close(&mut self) {
        if let Some(l) = &self.link {
            let _ = l.tx.send(Cmd::Close);
        }
    }

    fn poll_link(&mut self) {
        let Some(link) = &self.link else { return };
        let mut evs = Vec::new();
        while let Ok(e) = link.rx.try_recv() {
            evs.push(e);
            if evs.len() > 20_000 {
                break;
            }
        }
        let mut closed = false;
        for (at, e) in evs {
            match e {
                Ev::Info(s) => self.push(at, Kind::Info, s),
                Ev::Text(s) => self.push(at, Kind::Text, s),
                Ev::Error(s, lvl) => {
                    if s.starts_with("Bus ") || s.starts_with("Back to") {
                        self.bus_state = (s.split(',').next().unwrap_or("").to_string(), lvl);
                    }
                    self.push(at, if lvl >= 2 { Kind::Err } else { Kind::Warn }, s)
                }
                Ev::Rx(f) => {
                    self.rx_count += 1;
                    self.update_table(&f, at);
                    if self.paused {
                        continue;
                    }
                    if self.suppress && !self.seen.insert(f.clone()) {
                        continue;
                    }
                    self.push(at, Kind::Rx, f.display());
                }
                Ev::Tx(f) => {
                    self.tx_count += 1;
                    if !self.paused {
                        self.push(at, Kind::Tx, f.display());
                    }
                }
                Ev::Echo(f) => {
                    self.echo_count += 1;
                    if !self.paused {
                        self.push(at, Kind::Echo, f.display());
                    }
                }
                Ev::Busload(p) => {
                    self.busload = Some((p, at));
                    if p > 0 && !self.paused {
                        self.push(at, Kind::Info, format!("Busload: {p} %"));
                    }
                }
                Ev::Opened { fd, desc } => {
                    self.is_open = true;
                    self.link_fd = fd;
                    self.link_desc = desc;
                    self.bus_state = ("Bus Active".into(), 0);
                    self.push(at, Kind::Note, "Adapter is open".into());
                }
                Ev::Closed(err) => {
                    if let Some(e) = err {
                        self.push(at, Kind::Err, e);
                    }
                    self.push(at, Kind::Note, "Adapter closed".into());
                    closed = true;
                }
            }
        }
        if closed {
            self.link = None;
            self.is_open = false;
            self.identify = false;
            self.busload = None;
            self.bus_state = ("Closed".into(), 0);
        }
    }

    fn update_table(&mut self, f: &CanFrame, at: Instant) {
        let key = (f.ext, f.id);
        match self.table.get_mut(&key) {
            Some(s) => {
                let dt = at.saturating_duration_since(s.last).as_secs_f64() * 1000.0;
                s.period_ms = if s.count == 1 { dt } else { s.period_ms * 0.8 + dt * 0.2 };
                s.count += 1;
                s.last = at;
                s.frame = f.clone();
            }
            None => {
                self.table.insert(key, IdStat { frame: f.clone(), count: 1, last: at, period_ms: 0.0 });
            }
        }
    }

    fn build_frame(&self) -> Result<CanFrame, String> {
        let id = can::parse_hex_u32(&self.tx_id).ok_or("Enter the CAN ID in hex, e.g. 123 or 18DAF110")?;
        let ext = self.tx_ext || id > 0x7FF || self.tx_id.trim().len() > 3;
        if (!ext && id > 0x7FF) || id > 0x1FFF_FFFF {
            return Err("The CAN ID is too big".into());
        }
        let fd = self.tx_fd;
        if fd && !self.link_fd {
            return Err("CAN FD packets need 'CAN FD' to be ticked before opening.".into());
        }
        let mut data = if self.tx_rtr { Vec::new() } else { can::parse_hex_bytes(&self.tx_data)? };
        if self.tx_rtr {
            if fd {
                return Err("Remote frames do not exist in CAN FD".into());
            }
            let n = can::parse_hex_bytes(&self.tx_data).map(|v| v.len()).unwrap_or(0).min(8);
            data.push(n as u8);
        } else {
            if data.len() > if fd { 64 } else { 8 } {
                return Err(format!("Too many data bytes ({}), max {}", data.len(), if fd { 64 } else { 8 }));
            }
            can::pad(&mut data, fd, self.pad8);
        }
        Ok(CanFrame { id, ext, rtr: self.tx_rtr, fd, brs: fd && self.tx_brs, esi: false, data })
    }

    fn send(&mut self) {
        match self.build_frame() {
            Ok(f) => {
                if let Some(l) = &self.link {
                    let _ = l.tx.send(Cmd::Send(f));
                }
            }
            Err(e) => {
                self.repeat = false;
                self.note(Kind::Err, &e);
            }
        }
    }

    fn toggle_log(&mut self) {
        if let Some((mut w, p)) = self.logfile.take() {
            let _ = w.flush();
            self.note(Kind::Note, &format!("Logging stopped: {}", p.display()));
            return;
        }
        let name = format!("can-log-{}.txt", chrono::Local::now().format("%Y%m%d-%H%M%S"));
        if let Some(p) = rfd::FileDialog::new().set_file_name(&name).add_filter("Text", &["txt", "log"]).save_file() {
            match std::fs::File::create(&p) {
                Ok(f) => {
                    self.logfile = Some((std::io::BufWriter::new(f), p.clone()));
                    self.note(Kind::Note, &format!("Logging to {}", p.display()));
                }
                Err(e) => self.note(Kind::Err, &format!("Cannot create log file: {e}")),
            }
        }
    }

    // ------------------------------------------------------------------ firmware

    fn fw_busy(&self) -> bool {
        self.fw_rx.is_some()
    }

    fn fw_start(&mut self, ctx: &egui::Context, task: FwTask, what: &str) {
        if self.fw_busy() {
            return;
        }
        if self.link.is_some() {
            self.close();
            std::thread::sleep(Duration::from_millis(200));
            self.poll_link();
        }
        self.fw_log.push(Line { t: self.stamp(Instant::now()), kind: Kind::Note, text: what.to_string() });
        self.fw_progress = None;
        let c = ctx.clone();
        self.fw_rx = Some(device::run_fw(task, move || c.request_repaint()));
    }

    fn poll_fw(&mut self) {
        let Some(rx) = &self.fw_rx else { return };
        let mut evs = Vec::new();
        while let Ok(e) = rx.try_recv() {
            evs.push(e);
        }
        let mut done = false;
        for e in evs {
            match e {
                FwEv::Log(s, 9) => {
                    let mut p = s.split('|');
                    let k = p.next().unwrap_or("").to_string();
                    let b = p.next().unwrap_or("").to_string();
                    let m = p.next().unwrap_or("").to_string();
                    self.fw_board = Some((k, b, m));
                }
                FwEv::Log(s, lvl) => {
                    let kind = match lvl {
                        0 => Kind::Info,
                        1 => Kind::Warn,
                        _ => Kind::Err,
                    };
                    self.fw_log.push(Line { t: self.stamp(Instant::now()), kind, text: s });
                }
                FwEv::Progress(p, s) => self.fw_progress = Some((p, s)),
                FwEv::Done => done = true,
            }
        }
        if done {
            self.fw_rx = None;
            self.refresh();
        }
    }

    fn fw_matches(&self, f: &FwFile) -> bool {
        match &self.fw_board {
            Some((k, b, m)) if !b.is_empty() => {
                f.kind.starts_with(k.as_str()) && f.board.eq_ignore_ascii_case(b) && m.to_ascii_uppercase().starts_with(&f.mcu.to_ascii_uppercase())
            }
            _ => false,
        }
    }
}

fn baud_text(b: u32) -> String {
    if b == 0 {
        "Custom…".into()
    } else {
        format!("{} baud", crate::timing::fmt_baud(b as f64))
    }
}

fn group(ui: &mut egui::Ui, title: &str, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(th((246, 248, 252), (36, 40, 48)))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.label(RichText::new(title).strong().color(th((40, 70, 140), (130, 170, 240))));
                add(ui);
            });
        });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_link();
        self.poll_fw();

        if self.repeat && self.is_open && self.last_repeat.elapsed() >= Duration::from_millis(self.repeat_ms.max(1) as u64) {
            self.last_repeat = Instant::now();
            self.send();
        }
        if self.repeat && self.is_open {
            ctx.request_repaint_after(Duration::from_millis(self.repeat_ms.clamp(1, 1000) as u64));
        }
        if self.link.is_some() || self.fw_busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(Color32::from_rgb(34, 52, 92)).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("CAN Bench").size(20.0).strong().color(Color32::WHITE));
                    ui.add_space(6.0);
                    ui.label(RichText::new("CANable 2.5 terminal & firmware updater").color(Color32::from_rgb(180, 195, 225)));
                    ui.add_space(24.0);
                    for (t, name) in [(Tab::Terminal, "📟  CAN Terminal"), (Tab::Firmware, "⬆  Firmware Update")] {
                        let on = self.tab == t;
                        let b = egui::Button::new(RichText::new(name).color(if on { Color32::from_rgb(20, 30, 60) } else { Color32::WHITE }))
                            .fill(if on { Color32::from_rgb(220, 230, 250) } else { Color32::from_rgb(55, 76, 125) });
                        if ui.add(b).clicked() {
                            self.tab = t;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let dark = is_dark();
                        let tb = egui::Button::new(RichText::new(if dark { "☀  Light mode" } else { "🌙  Dark mode" }).color(Color32::WHITE))
                            .fill(Color32::from_rgb(55, 76, 125));
                        if ui.add(tb).on_hover_text("Switch between light and dark colours").clicked() {
                            apply_theme(&ctx, !dark);
                            let f = theme_file();
                            if let Some(d) = f.parent() {
                                let _ = std::fs::create_dir_all(d);
                            }
                            let _ = std::fs::write(f, if !dark { "dark" } else { "light" });
                        }
                        ui.add_space(12.0);
                        let (txt, col) = if self.is_open {
                            ("● Open", Color32::from_rgb(110, 230, 120))
                        } else if self.link.is_some() {
                            ("● Opening", Color32::from_rgb(250, 200, 80))
                        } else {
                            ("● Closed", Color32::from_rgb(190, 190, 200))
                        };
                        ui.label(RichText::new(txt).color(col).strong());
                    });
                });
            });

        match self.tab {
            Tab::Terminal => self.terminal_ui(ui, &ctx),
            Tab::Firmware => self.firmware_ui(ui, &ctx),
        }

        // --screenshot
        if let Some(path) = self.shot.clone() {
            self.frames += 1;
            ctx.request_repaint();
            if self.frames >= 120 && !self.shot_requested {
                self.shot_requested = true;
                let done = self.shot_done.clone();
                let c2 = ctx.clone();
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let mut raw = Vec::with_capacity(w * h * 4);
                    for p in &img.pixels {
                        raw.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
                    }
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                        let _ = buf.save(&path);
                    }
                    done.store(true, std::sync::atomic::Ordering::SeqCst);
                    c2.request_repaint();
                });
            }
            if self.shot_done.load(std::sync::atomic::Ordering::SeqCst) || self.frames > 600 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

impl App {
    fn terminal_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let locked = self.link.is_some();

        egui::Panel::top("settings").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).fill(th((236, 240, 247), (30, 33, 40)))).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                group(ui, "Adapter", 380.0, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(!locked, |ui| {
                            let text = self.adapters.get(self.sel).map(|a| a.label().to_string()).unwrap_or_else(|| "No adapter found".into());
                            egui::ComboBox::from_id_salt("adapter").width(280.0).selected_text(text).show_ui(ui, |ui| {
                                for (i, a) in self.adapters.iter().enumerate() {
                                    ui.selectable_value(&mut self.sel, i, a.label());
                                }
                            });
                            if ui.button("⟳ Refresh").on_hover_text("Search for connected adapters").clicked() {
                                self.refresh();
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        if !locked {
                            if ui.add(egui::Button::new(RichText::new("▶  Open").strong()).fill(th((200, 235, 205), (40, 90, 50)))).clicked() {
                                self.open(ctx);
                            }
                        } else if ui.add(egui::Button::new(RichText::new("■  Close").strong()).fill(th((245, 210, 205), (110, 45, 40)))).clicked() {
                            self.close();
                        }
                        ui.add_enabled_ui(self.is_open, |ui| {
                            if ui.toggle_value(&mut self.identify, "💡 Identify").on_hover_text("Blink the adapter's LEDs").changed() {
                                if let Some(l) = &self.link {
                                    let _ = l.tx.send(Cmd::Identify(self.identify));
                                }
                            }
                        });
                        ui.label("Mode:");
                        ui.add_enabled_ui(!locked, |ui| {
                            egui::ComboBox::from_id_salt("mode")
                                .selected_text(match self.mode {
                                    BusMode::Normal => "Normal",
                                    BusMode::ListenOnly => "Silent (sniff)",
                                    BusMode::LoopbackInternal => "Loopback internal",
                                    BusMode::LoopbackExternal => "Loopback external",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.mode, BusMode::Normal, "Normal");
                                    ui.selectable_value(&mut self.mode, BusMode::ListenOnly, "Silent (sniff)").on_hover_text("Listen only: packets are not acknowledged");
                                    ui.selectable_value(&mut self.mode, BusMode::LoopbackInternal, "Loopback internal");
                                    ui.selectable_value(&mut self.mode, BusMode::LoopbackExternal, "Loopback external");
                                });
                        });
                    });
                });
                group(ui, "Bus speed  (baudrate · samplepoint)", 320.0, |ui| {
                    ui.add_enabled_ui(!locked, |ui| {
                        egui::Grid::new("speed").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("Nominal");
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("nom").width(100.0).selected_text(baud_text(self.nom_baud)).show_ui(ui, |ui| {
                                    for b in NOMINAL {
                                        ui.selectable_value(&mut self.nom_baud, b, baud_text(b));
                                    }
                                });
                                if self.nom_baud == 0 {
                                    ui.add(egui::DragValue::new(&mut self.nom_custom).range(1_000..=1_000_000).suffix(" baud"));
                                }
                                ui.add(egui::DragValue::new(&mut self.nom_sp).range(50.0..=90.0).speed(0.5).suffix(" %")).on_hover_text("Samplepoint");
                            });
                            ui.end_row();
                            ui.checkbox(&mut self.fd, "CAN FD");
                            ui.add_enabled_ui(self.fd, |ui| {
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("data").width(100.0).selected_text(baud_text(self.data_baud)).show_ui(ui, |ui| {
                                        for b in DATA {
                                            ui.selectable_value(&mut self.data_baud, b, baud_text(b));
                                        }
                                    });
                                    if self.data_baud == 0 {
                                        ui.add(egui::DragValue::new(&mut self.data_custom).range(100_000..=10_000_000).suffix(" baud"));
                                    }
                                    ui.add(egui::DragValue::new(&mut self.data_sp).range(50.0..=80.0).speed(0.5).suffix(" %")).on_hover_text("Data samplepoint");
                                });
                            });
                            ui.end_row();
                        });
                    });
                });
                group(ui, "Options", 420.0, |ui| {
                    ui.add_enabled_ui(!locked, |ui| {
                        egui::Grid::new("opts").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                            ui.checkbox(&mut self.auto_retx, "Auto retransmit").on_hover_text("Off = one-shot: a packet that is not acknowledged is sent only once");
                            ui.checkbox(&mut self.echo, "Show Tx echo (ACK)").on_hover_text("Show sent packets again in olive when another node acknowledged them");
                            ui.end_row();
                            ui.checkbox(&mut self.termination, "120 Ω termination").on_hover_text("Only boards with a software switch");
                            ui.checkbox(&mut self.busload_on, "Busload report");
                            ui.end_row();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Host filter:");
                            ui.add(egui::TextEdit::singleline(&mut self.filters).desired_width(310.0).hint_text("e.g. 7E8,7FF; 18DA00F1,1FFF00FF"))
                                .on_hover_text("Up to 8 'ID,mask' pairs. Only matching IDs are sent from the adapter to the PC. Empty = everything.");
                        });
                    });
                });
            });
        });

        egui::Panel::top("send").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).fill(th((244, 246, 251), (34, 37, 45)))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Send").strong().color(th((40, 70, 140), (130, 170, 240))));
                ui.label("ID:");
                ui.add(egui::TextEdit::singleline(&mut self.tx_id).desired_width(80.0).font(egui::TextStyle::Monospace));
                ui.label("Data:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.tx_data).desired_width(300.0).font(egui::TextStyle::Monospace).hint_text("hex bytes"));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.checkbox(&mut self.tx_ext, "29 bit");
                ui.checkbox(&mut self.tx_rtr, "RTR");
                ui.add_enabled_ui(self.fd, |ui| {
                    ui.checkbox(&mut self.tx_fd, "FD");
                    ui.add_enabled(self.tx_fd, egui::Checkbox::new(&mut self.tx_brs, "BRS"));
                });
                ui.checkbox(&mut self.pad8, "Pad to 8");
                let send = ui.add_enabled(self.is_open, egui::Button::new(RichText::new("Send").strong()).fill(th((205, 220, 250), (45, 70, 120))));
                if send.clicked() || (enter && self.is_open) {
                    self.send();
                }
                ui.separator();
                ui.add_enabled(self.is_open, egui::Checkbox::new(&mut self.repeat, "Repeat every"));
                ui.add(egui::DragValue::new(&mut self.repeat_ms).range(1..=60_000).suffix(" ms"));
            });
            if matches!(self.selected(), Some(AdapterRef::Slcan { .. })) && self.is_open {
                ui.horizontal(|ui| {
                    ui.label("Slcan command:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.raw_cmd).desired_width(200.0).font(egui::TextStyle::Monospace).hint_text("e.g. V"));
                    if (ui.button("Send command").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && !self.raw_cmd.is_empty() {
                        if let Some(l) = &self.link {
                            let _ = l.tx.send(Cmd::Raw(self.raw_cmd.clone()));
                        }
                    }
                });
            }
        });

        egui::Panel::bottom("status").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 5)).fill(th((230, 234, 242), (26, 29, 35)))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Rx {}", self.rx_count)).color(C_RX()));
                ui.label(RichText::new(format!("Tx {}", self.tx_count)).color(C_TX()));
                ui.label(RichText::new(format!("ACK {}", self.echo_count)).color(C_ECHO()));
                ui.separator();
                let bl = self.busload.filter(|(_, t)| t.elapsed() < Duration::from_secs(5)).map(|b| b.0).unwrap_or(0);
                ui.label("Busload:");
                ui.add(egui::ProgressBar::new(bl as f32 / 100.0).desired_width(120.0).text(format!("{bl} %")));
                ui.separator();
                let col = match self.bus_state.1 {
                    0 => th((40, 120, 40), (110, 210, 110)),
                    1 => C_WARN(),
                    _ => C_ERR(),
                };
                ui.label(RichText::new(&self.bus_state.0).color(col).strong());
                if self.is_open {
                    ui.separator();
                    ui.label(RichText::new(&self.link_desc).small().color(C_TIME()));
                }
            });
        });

        egui::Panel::top("tools").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 4)).fill(th((250, 251, 253), (32, 35, 42)))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.view_table, false, "Trace");
                ui.selectable_value(&mut self.view_table, true, "Live ID table");
                ui.separator();
                if ui.button("Clear").clicked() {
                    self.trace.clear();
                    self.table.clear();
                    self.seen.clear();
                    self.rx_count = 0;
                    self.tx_count = 0;
                    self.echo_count = 0;
                }
                ui.checkbox(&mut self.autoscroll, "Scroll to bottom");
                ui.checkbox(&mut self.paused, "Pause");
                if ui.checkbox(&mut self.suppress, "Hide repeats").on_hover_text("Show each identical packet (same ID and data) only once").changed() {
                    self.seen.clear();
                }
                let logging = self.logfile.is_some();
                if ui.button(if logging { "⏹ Stop logging" } else { "⏺ Start logging" }).clicked() {
                    self.toggle_log();
                }
                if ui.button("💾 Save trace").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("trace.txt").save_file() {
                        let text: String = self.trace.iter().map(|l| format!("{}  {}\n", l.t, l.text)).collect();
                        if let Err(e) = std::fs::write(&p, text) {
                            self.note(Kind::Err, &format!("Cannot save: {e}"));
                        }
                    }
                }
                ui.separator();
                let r = ui.add(egui::TextEdit::singleline(&mut self.comment).desired_width(160.0).hint_text("comment"));
                if (ui.button("Comment").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && !self.comment.is_empty() {
                    let c = std::mem::take(&mut self.comment);
                    self.note(Kind::Note, &c);
                }
            });
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::Frame::new()
                .fill(th((255, 255, 255), (20, 22, 27)))
                .stroke(egui::Stroke::new(1.0, th((190, 198, 215), (70, 76, 90))))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    if self.view_table {
                        self.table_view(ui);
                    } else {
                        self.trace_view(ui);
                    }
                });
        });
    }

    fn trace_view(&mut self, ui: &mut egui::Ui) {
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        let n = self.trace.len();
        egui::ScrollArea::both().auto_shrink([false, false]).stick_to_bottom(self.autoscroll).show_rows(ui, row_h, n, |ui, range| {
            for l in &self.trace[range] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.label(RichText::new(&l.t).monospace().color(C_TIME()));
                    let mut t = RichText::new(&l.text).monospace().color(l.kind.color());
                    if l.kind == Kind::Note {
                        t = t.strong();
                    }
                    ui.label(t);
                });
            }
        });
    }

    fn table_view(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("ids").striped(true).num_columns(5).spacing([24.0, 3.0]).show(ui, |ui| {
                for h in ["ID", "Data", "Flags", "Count", "Period"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for s in self.table.values() {
                    let fresh = s.last.elapsed() < Duration::from_millis(300);
                    let col = if fresh { C_RX() } else { C_TEXT() };
                    ui.label(RichText::new(s.frame.id_text()).monospace().color(col));
                    ui.label(RichText::new(if s.frame.rtr { "RTR".into() } else { s.frame.data_text() }).monospace().color(col));
                    ui.label(RichText::new(s.frame.flags_text().trim().trim_matches(['(', ')'])).small());
                    ui.label(RichText::new(s.count.to_string()).monospace());
                    ui.label(RichText::new(if s.count > 1 { format!("{:.1} ms", s.period_ms) } else { "–".into() }).monospace());
                    ui.end_row();
                }
            });
            if self.table.is_empty() {
                ui.label(RichText::new("Received IDs appear here with their latest data, count and repeat period.").color(C_TIME()));
            }
        });
    }

    fn firmware_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.fw_busy();
        egui::Panel::bottom("fwlog").resizable(true).default_size(210.0).frame(egui::Frame::new().inner_margin(egui::Margin::same(10)).fill(th((236, 240, 247), (30, 33, 40)))).show(ui, |ui| {
            if let Some((p, s)) = &self.fw_progress {
                ui.add(egui::ProgressBar::new(*p).text(s.as_str()).show_percentage());
            } else {
                ui.add(egui::ProgressBar::new(0.0).text("Idle"));
            }
            egui::Frame::new().fill(th((255, 255, 255), (20, 22, 27))).stroke(egui::Stroke::new(1.0, th((190, 198, 215), (70, 76, 90)))).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                    if self.fw_log.is_empty() {
                        ui.label(RichText::new("Firmware messages appear here.").color(C_TIME()));
                    }
                    for l in &self.fw_log {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&l.t).monospace().color(C_TIME()));
                            ui.label(RichText::new(&l.text).color(l.kind.color()));
                        });
                    }
                });
            });
        });

        egui::Panel::left("fwdev").default_size(390.0).resizable(true).frame(egui::Frame::new().inner_margin(egui::Margin::same(10)).fill(th((244, 246, 251), (34, 37, 45)))).show(ui, |ui| {
            ui.label(RichText::new("1.  Adapter").strong().size(16.0).color(th((40, 70, 140), (130, 170, 240))));
            ui.horizontal(|ui| {
                let text = self.adapters.get(self.sel).map(|a| a.label().to_string()).unwrap_or_else(|| "No adapter found".into());
                egui::ComboBox::from_id_salt("fwadapter").width(290.0).selected_text(text).show_ui(ui, |ui| {
                    for (i, a) in self.adapters.iter().enumerate() {
                        if ui.selectable_value(&mut self.sel, i, a.label()).clicked() {
                            self.fw_board = None;
                        }
                    }
                });
                if ui.button("⟳").on_hover_text("Refresh").clicked() {
                    self.refresh();
                }
            });
            let sel = self.selected();
            let in_dfu = matches!(sel, Some(AdapterRef::Bootloader { .. }));
            ui.add_space(4.0);
            ui.label(RichText::new(match &sel {
                None => "Connect a CANable adapter by USB.".to_string(),
                Some(AdapterRef::Bootloader { .. }) => "Firmware update mode (DFU): ready to flash.".to_string(),
                Some(_) => "Normal mode: switch to DFU mode to flash it.".to_string(),
            }).color(if in_dfu { C_RX() } else { C_TEXT() }));
            if let Some((k, b, m)) = &self.fw_board {
                if !b.is_empty() {
                    ui.label(RichText::new(format!("Running: {k} firmware, board {b}, {m}")).color(C_INFO()));
                }
            }
            ui.add_space(6.0);
            ui.add_enabled_ui(!busy && sel.is_some(), |ui| {
                egui::Grid::new("fwbtn").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
                    let w = egui::vec2(175.0, 28.0);
                    if ui.add_sized(w, egui::Button::new("ℹ  Show details")).clicked() {
                        let a = sel.clone().unwrap();
                        self.fw_start(ctx, FwTask::Describe(a), "Reading adapter details…");
                    }
                    if ui.add_enabled_ui(!in_dfu, |ui| ui.add_sized(w, egui::Button::new(if self.fw_identify { "💡  Stop blinking" } else { "💡  Identify (blink)" }))).inner.clicked() {
                        self.fw_identify = !self.fw_identify;
                        let a = sel.clone().unwrap();
                        self.fw_start(ctx, FwTask::Identify(a, self.fw_identify), "Identify");
                    }
                    ui.end_row();
                    if ui.add_enabled_ui(!in_dfu, |ui| ui.add_sized(w, egui::Button::new("⬇  Enter DFU mode").fill(th((255, 236, 200), (110, 80, 30))))).inner.on_hover_text("Switch the adapter into firmware update mode (no jumper needed with CANable 2.5 firmware)").clicked() {
                        let a = sel.clone().unwrap();
                        self.fw_start(ctx, FwTask::EnterDfu(a), "Enter DFU mode");
                    }
                    if ui.add_enabled_ui(in_dfu, |ui| ui.add_sized(w, egui::Button::new("⬆  Leave DFU mode"))).inner.on_hover_text("Restart the adapter with its firmware").clicked() {
                        self.fw_start(ctx, FwTask::Leave, "Leave DFU mode");
                    }
                    ui.end_row();
                    if ui.add_enabled_ui(!in_dfu, |ui| ui.add_sized(w, egui::Button::new("⏚  Disable pin BOOT0"))).inner.on_hover_text("For boards that fall into DFU mode by themselves (e.g. MKS Makerbase, Oleksii). 'Enter DFU mode' re-enables it.").clicked() {
                        let a = sel.clone().unwrap();
                        self.fw_start(ctx, FwTask::DisableBoot0(a), "Disable pin BOOT0");
                    }
                    if ui.add_enabled_ui(in_dfu, |ui| ui.add_sized(w, egui::Button::new("💾  Back up firmware"))).inner.on_hover_text("Read the current firmware into a .dfu file").clicked() {
                        let name = format!("Backup {}.dfu", chrono::Local::now().format("%Y-%m-%d %H-%M"));
                        if let Some(p) = rfd::FileDialog::new().set_directory(&self.own_fw_dir).set_file_name(&name).add_filter("DFU", &["dfu"]).save_file() {
                            self.fw_start(ctx, FwTask::Backup(p), "Backing up the current firmware");
                        }
                    }
                    ui.end_row();
                });
            });
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("How it works").strong());
            ui.label(RichText::new(
                "1. Pick your adapter and click 'Enter DFU mode'.\n\
                 2. Pick the firmware made for your board (Slcan = COM port, Candlelight = USB).\n\
                 3. Click 'Flash firmware'. It is erased, written and verified, then the adapter restarts.\n\
                 The STM32 bootloader cannot be damaged, so a failed update can simply be repeated.",
            ).small().color(th((70, 75, 90), (175, 180, 195))));
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("2.  Firmware file").strong().size(16.0).color(th((40, 70, 140), (130, 170, 240))));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy, egui::Button::new("🌐 Get latest from GitHub")).on_hover_text("Download the newest CANable 2.5 .dfu files").clicked() {
                        let d = self.own_fw_dir.clone();
                        self.fw_start(ctx, FwTask::DownloadLatest(d), "Downloading the latest firmware");
                    }
                    if ui.button("➕ Add file…").on_hover_text("Copy a .dfu or .bin file into the firmware folder").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Firmware", &["dfu", "bin"]).pick_file() {
                            let _ = std::fs::create_dir_all(&self.own_fw_dir);
                            if let Some(n) = p.file_name() {
                                if let Err(e) = std::fs::copy(&p, self.own_fw_dir.join(n)) {
                                    self.fw_log.push(Line { t: self.stamp(Instant::now()), kind: Kind::Err, text: format!("Copy failed: {e}") });
                                }
                            }
                            self.scan_firmware();
                        }
                    }
                    if ui.button("⟳").on_hover_text("Rescan the firmware folders").clicked() {
                        self.scan_firmware();
                    }
                    if ui.button("📂").on_hover_text("Open the firmware folder").clicked() {
                        let _ = std::fs::create_dir_all(&self.own_fw_dir);
                        let _ = std::process::Command::new(if cfg!(windows) { "explorer" } else { "xdg-open" }).arg(&self.own_fw_dir).spawn();
                    }
                });
            });
            let dirs: Vec<String> = self.fw_dirs.iter().map(|d| d.display().to_string()).collect();
            ui.label(RichText::new(format!("Folders: {}", dirs.join("  |  "))).small().color(C_TIME()));
            ui.add_space(4.0);
            egui::Frame::new().fill(th((255, 255, 255), (20, 22, 27))).stroke(egui::Stroke::new(1.0, th((190, 198, 215), (70, 76, 90)))).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.set_min_height((ui.available_height() - 60.0).max(120.0));
                egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - 50.0).show(ui, |ui| {
                    egui::Grid::new("fwfiles").striped(true).num_columns(5).spacing([18.0, 4.0]).show(ui, |ui| {
                        for h in ["Firmware", "Board", "Processor", "Size", ""] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        let mut clicked = None;
                        for (i, f) in self.fw_files.iter().enumerate() {
                            let m = self.fw_matches(f);
                            let label = if f.kind.is_empty() { f.name.clone() } else { f.kind.clone() };
                            let mut rt = RichText::new(label);
                            if m {
                                rt = rt.strong().color(C_RX());
                            }
                            if ui.selectable_label(self.fw_sel == Some(i), rt).on_hover_text(f.path.display().to_string()).clicked() {
                                clicked = Some(i);
                            }
                            ui.label(&f.board);
                            ui.label(&f.mcu);
                            ui.label(if f.size > 0 { format!("{:.1} KB", f.size as f32 / 1024.0) } else { "–".into() });
                            if let Some(e) = &f.err {
                                ui.label(RichText::new(e).color(C_ERR()).small());
                            } else if m {
                                ui.label(RichText::new("✔ matches your adapter").color(C_RX()).small());
                            } else {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                        if let Some(i) = clicked {
                            self.fw_sel = Some(i);
                        }
                    });
                    if self.fw_files.is_empty() {
                        ui.label(RichText::new("No firmware files yet. Click 'Get latest from GitHub' or 'Add file…'.").color(C_TIME()));
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let sel_dfu = matches!(self.selected(), Some(AdapterRef::Bootloader { .. }));
                let file_ok = self.fw_sel.and_then(|i| self.fw_files.get(i)).is_some_and(|f| f.file.is_some());
                let b = egui::Button::new(RichText::new("⚡  Flash firmware").strong().size(15.0)).fill(th((190, 225, 195), (40, 95, 55))).min_size(egui::vec2(170.0, 32.0));
                let resp = ui.add_enabled(!busy && file_ok && sel_dfu, b);
                if !sel_dfu {
                    resp.clone().on_disabled_hover_text("Switch the adapter into DFU mode first");
                }
                if resp.clicked() {
                    let f = &self.fw_files[self.fw_sel.unwrap()];
                    let file = f.file.clone().unwrap();
                    let msg = format!("Flashing {}", f.name);
                    let leave = self.fw_leave;
                    self.fw_start(ctx, FwTask::Flash(file, leave), &msg);
                }
                ui.checkbox(&mut self.fw_leave, "Leave DFU mode afterwards");
                if let Some(f) = self.fw_sel.and_then(|i| self.fw_files.get(i)) {
                    ui.label(RichText::new(format!("Selected: {}", f.name)).color(C_TIME()));
                }
            });
        });
    }
}
