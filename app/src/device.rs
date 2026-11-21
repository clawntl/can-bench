//! Adapter discovery and the background workers (terminal connection, firmware tasks).

use crate::can::{CanFrame, HostFilter};
use crate::timing::{self, Limits, Timing};
use crate::{candle, dfu, dfufile, slcan};
use nusb::MaybeFuture;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum AdapterRef {
    /// Candlelight channel: USB device id + interface number + channel index
    Candle { id: nusb::DeviceId, interface: u8, channel: u8, label: String },
    /// Slcan COM port
    Slcan { port: String, label: String },
    /// STM32 bootloader in DFU mode
    Bootloader { id: nusb::DeviceId, label: String },
}

impl AdapterRef {
    pub fn label(&self) -> &str {
        match self {
            AdapterRef::Candle { label, .. } | AdapterRef::Slcan { label, .. } | AdapterRef::Bootloader { label, .. } => label,
        }
    }
}

pub fn scan() -> Vec<AdapterRef> {
    let mut out = Vec::new();
    if let Ok(list) = nusb::list_devices().wait() {
        for d in list {
            let vp = (d.vendor_id(), d.product_id());
            if candle::IDS.contains(&vp) {
                let serial = d.serial_number().unwrap_or("").to_string();
                let product = d.product_string().unwrap_or("Candlelight").to_string();
                let mut chans: Vec<u8> = d
                    .interfaces()
                    .filter(|i| i.class() == 0xFF && i.interface_number() != candle::DFU_INTERFACE)
                    .map(|i| i.interface_number())
                    .collect();
                if chans.is_empty() {
                    chans.push(0);
                }
                let n = chans.len();
                for intf in chans {
                    let ch = if intf == 0 { 0 } else { intf - 1 };
                    let label = if n > 1 {
                        format!("{product} – channel {} – {serial}", ch + 1)
                    } else {
                        format!("{product} – {serial}")
                    };
                    out.push(AdapterRef::Candle { id: d.id(), interface: intf, channel: ch, label });
                }
            } else if vp == (dfu::VID, dfu::PID) {
                out.push(AdapterRef::Bootloader { id: d.id(), label: format!("STM32 bootloader (DFU mode) – {}", d.serial_number().unwrap_or("")) });
            }
        }
    }
    if let Ok(ports) = serialport::available_ports() {
        for p in ports {
            if let serialport::SerialPortType::UsbPort(u) = &p.port_type {
                let known = slcan::IDS.contains(&(u.vid, u.pid));
                let prod = u.product.clone().unwrap_or_else(|| "USB serial".into());
                let label = if known {
                    format!("{} – {}", p.port_name, prod)
                } else {
                    format!("{} – {} (not a known CANable)", p.port_name, prod)
                };
                out.push(AdapterRef::Slcan { port: p.port_name.clone(), label });
            }
        }
    }
    out
}

fn open_usb(id: nusb::DeviceId) -> Result<nusb::Device, String> {
    let info = nusb::list_devices()
        .wait()
        .map_err(|e| e.to_string())?
        .find(|d| d.id() == id)
        .ok_or("The adapter is not connected anymore")?;
    info.open().wait().map_err(|e| format!("Cannot open the USB device: {e}"))
}

// ===================================================================== terminal

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusMode {
    Normal,
    ListenOnly,
    LoopbackInternal,
    LoopbackExternal,
}

#[derive(Clone, Debug)]
pub struct OpenConfig {
    pub adapter: AdapterRef,
    pub nominal: (u32, f64),
    pub data: Option<(u32, f64)>,
    pub mode: BusMode,
    pub auto_retransmit: bool,
    pub echo: bool,
    pub termination: bool,
    pub busload: bool,
    pub filters: Vec<HostFilter>,
}

pub enum Cmd {
    Send(CanFrame),
    Identify(bool),
    Raw(String),
    Close,
}

#[derive(Debug)]
pub enum Ev {
    Info(String),
    Text(String),
    Error(String, u8),
    Rx(CanFrame),
    Tx(CanFrame),
    Echo(CanFrame),
    Busload(u8),
    Opened { fd: bool, desc: String },
    Closed(Option<String>),
}

pub struct Link {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<(Instant, Ev)>,
}

pub fn open(cfg: OpenConfig, repaint: impl Fn() + Send + 'static) -> Link {
    let (ctx, crx) = channel::<Cmd>();
    let (etx, erx) = channel::<(Instant, Ev)>();
    std::thread::spawn(move || {
        let send = |e: Ev| {
            let _ = etx.send((Instant::now(), e));
            repaint();
        };
        let r = match &cfg.adapter {
            AdapterRef::Candle { .. } => run_candle(&cfg, &crx, &send),
            AdapterRef::Slcan { .. } => run_slcan(&cfg, &crx, &send),
            AdapterRef::Bootloader { .. } => Err("This adapter is in firmware update (DFU) mode. Leave DFU mode on the Firmware page first.".into()),
        };
        send(Ev::Closed(r.err()));
    });
    Link { tx: ctx, rx: erx }
}

fn timing_desc(clock: u32, n: &Timing, d: Option<&Timing>) -> String {
    let mut s = format!("Nominal: {} baud, {:.1} % (prescaler {}, seg1 {}, seg2 {})", timing::fmt_baud(n.baud(clock)), n.samplepoint(), n.brp, n.seg1, n.seg2);
    if let Some(d) = d {
        s += &format!(
            ";  Data: {} baud, {:.1} % (prescaler {}, seg1 {}, seg2 {}); same prescaler: {}",
            timing::fmt_baud(d.baud(clock)),
            d.samplepoint(),
            d.brp,
            d.seg1,
            d.seg2,
            if n.brp == d.brp { "Yes" } else { "No" }
        );
    }
    s
}

fn calc_timings(cfg: &OpenConfig, clock: u32, ln: Limits, ld: Option<Limits>, send: &dyn Fn(Ev)) -> Result<(Timing, Option<Timing>), String> {
    match (cfg.data, ld) {
        (Some(d), Some(ld)) => {
            let (n, dt, perfect) = timing::calc_fd(clock, cfg.nominal, d, &ln, &ld)?;
            if !perfect {
                send(Ev::Error(
                    format!("No perfect match: using {:.1} % / {:.1} % samplepoint.", n.samplepoint(), dt.samplepoint()),
                    1,
                ));
            }
            Ok((n, Some(dt)))
        }
        (Some(_), None) => Err("This adapter does not support CAN FD.".into()),
        (None, _) => {
            let (n, exact) = timing::calc(clock, cfg.nominal.0, cfg.nominal.1, &ln)?;
            if !exact {
                send(Ev::Error(format!("Samplepoint {:.1} % is not possible, using {:.1} %", cfg.nominal.1, n.samplepoint()), 1));
            }
            Ok((n, None))
        }
    }
}

fn run_candle(cfg: &OpenConfig, crx: &Receiver<Cmd>, send: &dyn Fn(Ev)) -> Result<(), String> {
    let AdapterRef::Candle { id, interface, channel, .. } = cfg.adapter.clone() else { unreachable!() };
    let dev = open_usb(id)?;
    let intf = dev
        .claim_interface(interface)
        .wait()
        .map_err(|e| format!("Cannot claim the adapter (is it open in another program?): {e}"))?;
    let dd = dev.device_descriptor();
    send(Ev::Info(format!("USB device VID {:04X}, PID {:04X}, interface {}", dd.vendor_id(), dd.product_id(), interface)));
    let info = candle::read_info(&intf, channel)?;
    for l in candle::info_lines(&info, channel) {
        send(Ev::Info(l));
    }
    let ln = info.nominal.unwrap_or(Limits::nominal_g4());
    let (n, d) = calc_timings(cfg, info.fclk, ln, if info.fd() { info.data } else { None }, send)?;
    candle::set_timing(&intf, channel, false, &n)?;
    if let Some(d) = &d {
        candle::set_timing(&intf, channel, true, d)?;
    }
    let desc = timing_desc(info.fclk, &n, d.as_ref());
    send(Ev::Info(desc.clone()));
    if info.elmue() {
        for f in &cfg.filters {
            candle::add_filter(&intf, channel, f)?;
            send(Ev::Info(format!("Host filter: ID {:X}, mask {:X} ({} bit)", f.id, f.mask, if f.ext { 29 } else { 11 })));
        }
        if cfg.busload {
            candle::ctrl_out(&intf, candle::REQ_SET_BUSLOAD, channel as u16, &[10])?; // every 1 s
        }
    } else if !cfg.filters.is_empty() {
        send(Ev::Error("Host filters need the CANable 2.5 firmware – ignored.".into(), 1));
    }
    if info.feature & candle::F_TERMINATION != 0 {
        let _ = candle::ctrl_out(&intf, candle::REQ_SET_TERMINATION, channel as u16, &(cfg.termination as u32).to_le_bytes());
    } else if cfg.termination {
        send(Ev::Error("This board cannot switch the 120 Ω resistor by software.".into(), 1));
    }
    if !info.elmue() {
        return Err("This program needs the CANable 2.5 firmware (ElmüSoft). Update the adapter on the Firmware page.".into());
    }
    // firmware dates checked by HUD's open routine (ECU_Hacker 0x06002144)
    if info.sw_version <= candle::FW_MINIMUM {
        send(Ev::Error(format!("Firmware {} is old (13 Jan 2026 or earlier). Please update it on the Firmware page.", candle::bcd(info.sw_version)), 1));
    } else if info.sw_version < candle::FW_LATEST_KNOWN {
        send(Ev::Info(format!("A newer firmware than {} is available on the Firmware page.", candle::bcd(info.sw_version))));
    }
    // every flag must be in the adapter's capabilities (ECU_Hacker 0x06002145)
    let mut flags = 0u32;
    let mut want = |f: u32, what: &str| -> Result<(), String> {
        if info.feature & f == 0 {
            return Err(format!("The adapter does not support {what} (flag 0x{f:X})."));
        }
        flags |= f;
        Ok(())
    };
    want(candle::F_ELMUE, "the ElmüSoft protocol")?;
    if info.feature & candle::F_BLOBS != 0 {
        want(candle::F_BLOBS, "USB blobs")?;
    }
    if info.feature & candle::F_ERROR_REPORTS != 0 {
        want(candle::F_ERROR_REPORTS, "error reports")?;
    }
    if !cfg.auto_retransmit {
        want(candle::F_ONE_SHOT, "one-shot mode")?;
    }
    if d.is_some() {
        want(candle::F_CAN_FD, "CAN FD")?;
    }
    match cfg.mode {
        BusMode::Normal => {}
        BusMode::ListenOnly => want(candle::F_LISTEN_ONLY, "silent mode")?,
        BusMode::LoopbackInternal => {
            want(candle::F_LISTEN_ONLY, "silent mode")?;
            want(candle::F_LOOPBACK, "loopback")?;
        }
        BusMode::LoopbackExternal => want(candle::F_LOOPBACK, "loopback")?,
    }
    candle::set_mode(&intf, channel, 1, flags)?;
    let mut c = candle::Candle::open(intf, channel)?;
    send(Ev::Opened { fd: d.is_some(), desc });

    let mut marker: u8 = 0;
    let mut echoed: u8 = 0;
    let mut sent: Vec<Option<CanFrame>> = vec![None; 256];
    let mut errors = 0;
    loop {
        loop {
            match crx.try_recv() {
                Ok(Cmd::Send(f)) => {
                    // free Tx slots = 64 - (tx marker - last echoed marker), wrap at 255 (ECU_Hacker 0x0600214e)
                    if cfg.echo && candle::free_tx_slots(marker, echoed) == 0 {
                        send(Ev::Error(candle::feedback_text(b'7'), 2));
                        continue;
                    }
                    let m = if cfg.echo {
                        marker = if marker == 255 { 1 } else { marker + 1 };
                        sent[marker as usize] = Some(f.clone());
                        marker
                    } else {
                        0
                    };
                    match c.write(&candle::tx_bytes(&f, m)) {
                        Ok(()) => send(Ev::Tx(f)),
                        Err(e) => send(Ev::Error(e, 2)),
                    }
                }
                Ok(Cmd::Identify(on)) => {
                    // needs capability flag 0x20 (ECU_Hacker 0x06002148)
                    if info.feature & candle::F_IDENTIFY == 0 {
                        send(Ev::Error("This adapter cannot blink its LEDs.".into(), 1));
                    } else if let Err(e) = candle::identify(&c.intf, channel, on) {
                        send(Ev::Error(e, 1));
                    }
                }
                Ok(Cmd::Raw(_)) => send(Ev::Error("Raw commands are only for Slcan adapters.".into(), 1)),
                Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = candle::set_mode(&c.intf, channel, 0, candle::F_ELMUE);
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        match c.poll(Duration::from_millis(10)) {
            Ok(Some(buf)) => {
                errors = 0;
                for m in candle::parse_in(&buf) {
                    match m {
                        candle::InMsg::Rx(f) => send(Ev::Rx(f)),
                        candle::InMsg::Echo(mk) => {
                            echoed = mk;
                            if let Some(f) = sent[mk as usize].take() {
                                send(Ev::Echo(f));
                            }
                        }
                        candle::InMsg::Error { err_id, data } => {
                            let (t, lvl) = candle::error_text(err_id, &data);
                            send(Ev::Error(t, lvl));
                        }
                        candle::InMsg::Text(t) => send(Ev::Text(t)),
                        candle::InMsg::Busload(p) => send(Ev::Busload(p)),
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                errors += 1;
                if errors > 20 {
                    return Err(format!("{e} – the adapter was probably disconnected."));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn run_slcan(cfg: &OpenConfig, crx: &Receiver<Cmd>, send: &dyn Fn(Ev)) -> Result<(), String> {
    let AdapterRef::Slcan { port, .. } = &cfg.adapter else { unreachable!() };
    let mut s = slcan::Slcan::open(port)?;
    let info = s.init()?;
    for l in slcan::info_lines(&info) {
        send(Ev::Info(l));
    }
    let mut other = Vec::new();
    let desc;
    let fd;
    if info.elmue {
        let mut modes = String::from("MDEF");
        modes.push(if cfg.auto_retransmit { 'A' } else { 'a' });
        if cfg.echo {
            modes.push('M');
        } else {
            modes.push('m');
        }
        modes.push('S');
        s.command(&modes, &mut other)?;
        let r = s.command(if cfg.termination { "MR" } else { "Mr" }, &mut other);
        if cfg.termination {
            if let Err(e) = r {
                send(Ev::Error(format!("120 Ω resistor: {e}"), 1));
            }
        }
        let clock = info.clock_mhz.max(1) * 1_000_000;
        let ld = if info.slcan_ver > 0 { Some(info.data.unwrap_or(Limits::data_g4())) } else { info.data };
        let (n, d) = calc_timings(cfg, clock, info.nominal.unwrap_or(Limits::nominal_g4()), ld, send)?;
        s.command(&format!("s{},{},{},{}", n.brp, n.seg1, n.seg2, n.sjw), &mut other)?;
        if let Some(d) = &d {
            s.command(&format!("y{},{},{},{}", d.brp, d.seg1, d.seg2, d.sjw), &mut other)?;
        }
        desc = timing_desc(clock, &n, d.as_ref());
        fd = d.is_some();
        if !cfg.filters.is_empty() {
            let f: Vec<String> = cfg.filters.iter().map(|f| if f.ext { format!("{:08X},{:08X}", f.id, f.mask) } else { format!("{:03X},{:03X}", f.id, f.mask) }).collect();
            s.command(&format!("F{}", f.join(";")), &mut other)?;
        }
        if cfg.busload {
            s.command("L10", &mut other)?;
        }
        let open = match cfg.mode {
            BusMode::Normal => "ON",
            BusMode::ListenOnly => "OS",
            BusMode::LoopbackInternal => "OI",
            BusMode::LoopbackExternal => "OE",
        };
        s.command(open, &mut other)?;
    } else {
        let c = slcan::legacy_s(cfg.nominal.0).ok_or("This legacy Slcan firmware only knows the standard baudrates (10k … 1M).")?;
        s.command(&format!("S{c}"), &mut other)?;
        fd = if let Some(d) = cfg.data {
            let y = slcan::legacy_y(d.0).ok_or("Unsupported data baudrate for legacy Slcan")?;
            s.command(&format!("Y{y}"), &mut other)?;
            true
        } else {
            false
        };
        if cfg.mode == BusMode::ListenOnly {
            s.command("M1", &mut other)?;
        }
        s.command("O", &mut other)?;
        desc = format!("Nominal {} baud (legacy Slcan, fixed samplepoint)", timing::fmt_baud(cfg.nominal.0 as f64));
    }
    send(Ev::Info(desc.clone()));
    send(Ev::Opened { fd, desc });

    let mut marker: u8 = 0;
    let mut sent: Vec<Option<CanFrame>> = vec![None; 256];
    let use_marker = info.elmue && cfg.echo;
    let handle = |l: String, sent: &mut Vec<Option<CanFrame>>| {
        let t = l.trim_start_matches(['&', '$']);
        if t.is_empty() {
            return;
        }
        if let Some(f) = slcan::parse_frame(t) {
            send(Ev::Rx(f));
        } else if let Some(rest) = t.strip_prefix('>') {
            send(Ev::Text(rest.to_string()));
        } else if let Some(rest) = t.strip_prefix('E') {
            let (txt, lvl) = slcan::error_text(rest);
            send(Ev::Error(txt, lvl));
        } else if let Some(rest) = t.strip_prefix('L') {
            if let Ok(p) = rest.parse() {
                send(Ev::Busload(p));
            }
        } else if let Some(rest) = t.strip_prefix('M') {
            if let Ok(m) = u8::from_str_radix(rest, 16) {
                if let Some(f) = sent[m as usize].take() {
                    send(Ev::Echo(f));
                }
            }
        } else if let Some(rest) = t.strip_prefix('#') {
            if let Some(c) = rest.bytes().next() {
                send(Ev::Error(candle::feedback_text(c), 2));
            }
        } else if t.ends_with('\x07') {
            send(Ev::Error("The adapter refused a command".into(), 2));
        } else {
            send(Ev::Text(t.to_string()));
        }
    };
    for l in other.drain(..) {
        handle(l, &mut sent);
    }
    loop {
        loop {
            match crx.try_recv() {
                Ok(Cmd::Send(f)) => {
                    let m = if use_marker {
                        marker = if marker == 255 { 1 } else { marker + 1 };
                        sent[marker as usize] = Some(f.clone());
                        Some(marker)
                    } else {
                        None
                    };
                    let cmd = slcan::frame_cmd(&f, m);
                    match s.send_raw(&format!("{cmd}\r")) {
                        Ok(()) => send(Ev::Tx(f)),
                        Err(e) => send(Ev::Error(e, 2)),
                    }
                }
                Ok(Cmd::Identify(on)) => {
                    let _ = s.send_raw(if on { "MI\r" } else { "Mi\r" });
                }
                Ok(Cmd::Raw(r)) => {
                    send(Ev::Info(format!("> {r}")));
                    let _ = s.send_raw(&format!("{r}\r"));
                }
                Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = s.send_raw("C\r");
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        s.pump().map_err(|e| format!("{e} – the adapter was probably disconnected."))?;
        let lines: Vec<String> = s.pending.drain(..).collect();
        for l in lines {
            // plain "#" success feedback after each sent frame is not interesting
            if l == "#" || l.is_empty() {
                continue;
            }
            handle(l, &mut sent);
        }
    }
}

// ===================================================================== firmware tasks

pub enum FwTask {
    Describe(AdapterRef),
    Identify(AdapterRef, bool),
    EnterDfu(AdapterRef),
    DisableBoot0(AdapterRef),
    Flash(dfufile::DfuFile, bool),
    Backup(PathBuf),
    Leave,
    DownloadLatest(PathBuf),
}

#[derive(Debug)]
pub enum FwEv {
    Log(String, u8),
    Progress(f32, String),
    Done,
}

pub fn run_fw(task: FwTask, repaint: impl Fn() + Send + 'static) -> Receiver<FwEv> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let log = |s: String, l: u8| {
            let _ = tx.send(FwEv::Log(s, l));
            repaint();
        };
        let prog = |p: f32, s: String| {
            let _ = tx.send(FwEv::Progress(p, s));
            repaint();
        };
        let r = fw_task(task, &log, &prog);
        if let Err(e) = r {
            log(e, 2);
        }
        let _ = tx.send(FwEv::Done);
        repaint();
    });
    rx
}

fn with_candle<T>(id: nusb::DeviceId, interface: u8, f: impl FnOnce(&nusb::Interface) -> Result<T, String>) -> Result<T, String> {
    let dev = open_usb(id)?;
    let intf = dev.claim_interface(interface).wait().map_err(|e| format!("Cannot claim the adapter (close it in the terminal first): {e}"))?;
    f(&intf)
}

fn find_bootloader() -> Result<nusb::Device, String> {
    let info = nusb::list_devices()
        .wait()
        .map_err(|e| e.to_string())?
        .find(|d| d.vendor_id() == dfu::VID && d.product_id() == dfu::PID)
        .ok_or("No adapter in DFU mode was found. Click 'Enter DFU mode' first (or reconnect it with the BOOT jumper set).")?;
    info.open().wait().map_err(|e| format!("Cannot open the bootloader: {e}"))
}

fn fw_task(task: FwTask, log: &dyn Fn(String, u8), prog: &dyn Fn(f32, String)) -> Result<(), String> {
    // "kind|board|mcu" is passed through the log channel with level 9
    let board = |s: String| log(s, 9);
    match task {
        FwTask::Describe(a) => match a {
            AdapterRef::Candle { id, interface, channel, .. } => with_candle(id, interface, |i| {
                let info = candle::read_info(i, channel)?;
                for l in candle::info_lines(&info, channel) {
                    log(l, 0);
                }
                board(format!("Candlelight|{}|{}", info.board, info.mcu));
                Ok(())
            }),
            AdapterRef::Slcan { port, .. } => {
                let mut s = slcan::Slcan::open(&port)?;
                let info = s.init()?;
                for l in slcan::info_lines(&info) {
                    log(l, 0);
                }
                board(format!("Slcan|{}|{}", info.board, info.mcu));
                if info.elmue {
                    let mut o = Vec::new();
                    if let Ok(b) = s.command("*Boot0:?", &mut o) {
                        log(format!("Pin BOOT0: {}", if b.trim_start_matches('+') == "1" { "enabled" } else { "disabled" }), 0);
                    }
                }
                Ok(())
            }
            AdapterRef::Bootloader { .. } => {
                let dev = find_bootloader()?;
                let d = dfu::Dfu::open(&dev)?;
                log(
                    format!(
                        "Bootloader: {} at 0x{:08X}, {} pages of {} bytes ({} KB), transfer size {} bytes",
                        d.layout.name,
                        d.layout.start,
                        d.layout.pages,
                        d.layout.page_size,
                        d.layout.size() / 1024,
                        d.transfer
                    ),
                    0,
                );
                Ok(())
            }
        },
        FwTask::Identify(a, on) => match a {
            AdapterRef::Candle { id, interface, channel, .. } => with_candle(id, interface, |i| candle::identify(i, channel, on)),
            AdapterRef::Slcan { port, .. } => {
                let mut s = slcan::Slcan::open(&port)?;
                let info = s.init()?;
                if !info.elmue {
                    return Err("Identify needs the CANable 2.5 firmware.".into());
                }
                let mut o = Vec::new();
                s.command(if on { "MI" } else { "Mi" }, &mut o).map(|_| ())
            }
            _ => Err("The bootloader cannot blink its LEDs.".into()),
        }
        .map(|_| log(if on { "The adapter LEDs are blinking now." } else { "Blinking stopped." }.into(), 0)),
        FwTask::DisableBoot0(a) => {
            match a {
                AdapterRef::Candle { id, interface, channel, .. } => with_candle(id, interface, |i| candle::disable_boot0(i, channel))?,
                AdapterRef::Slcan { port, .. } => {
                    let mut s = slcan::Slcan::open(&port)?;
                    let info = s.init()?;
                    if !info.elmue {
                        return Err("This feature needs the CANable 2.5 firmware from ElmüSoft.".into());
                    }
                    let mut o = Vec::new();
                    s.command("*Boot0:Off", &mut o)?;
                }
                _ => return Err("Leave DFU mode first.".into()),
            }
            log("The boot pin has been disabled. The adapter will no longer start in DFU mode by itself; 'Enter DFU mode' still works.".into(), 0);
            Ok(())
        }
        FwTask::EnterDfu(a) => {
            match a {
                AdapterRef::Candle { id, .. } => {
                    let ok = with_candle(id, candle::DFU_INTERFACE, candle::enter_dfu)?;
                    if !ok {
                        log("Please reconnect the USB cable now (pin BOOT0 was disabled, a hardware reset is needed).".into(), 1);
                        return Ok(());
                    }
                }
                AdapterRef::Slcan { port, .. } => {
                    let mut s = slcan::Slcan::open(&port)?;
                    let info = s.init()?;
                    if !info.elmue {
                        return Err("The adapter has a legacy firmware. Enter DFU mode manually: reconnect it with the BOOT jumper set / BOOT button pressed.".into());
                    }
                    let mut o = Vec::new();
                    if let Err(e) = s.command("*DFU", &mut o) {
                        if e.contains("reconnect") {
                            log("Please reconnect the USB cable now (pin BOOT0 was disabled, a hardware reset is needed).".into(), 1);
                            return Ok(());
                        }
                        return Err(e);
                    }
                }
                AdapterRef::Bootloader { .. } => {
                    log("The adapter is already in DFU mode.".into(), 0);
                    return Ok(());
                }
            }
            log("Switching to DFU mode…".into(), 0);
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(250));
                if find_bootloader().is_ok() {
                    log("The adapter is in DFU mode and ready for a firmware update.".into(), 0);
                    return Ok(());
                }
            }
            let present = nusb::list_devices().wait().map(|mut l| l.any(|d| d.vendor_id() == dfu::VID && d.product_id() == dfu::PID)).unwrap_or(false);
            if present {
                Err("The adapter is in DFU mode, but the bootloader cannot be opened. It needs the WinUSB driver for \"STM32 BOOTLOADER\".".into())
            } else {
                Err("The adapter did not appear in DFU mode. Reconnect it with the BOOT jumper set.".into())
            }
        }
        FwTask::Flash(file, leave) => {
            let dev = find_bootloader()?;
            let d = dfu::Dfu::open(&dev)?;
            let t = file.targets.iter().find(|t| t.alt_setting == 0).ok_or("The DFU file has no data for the internal flash (target 0).")?;
            let total: usize = t.elements.iter().map(|e| e.data.len()).sum();
            log(format!("Firmware: {} bytes in {} block(s). Flash: {} KB", total, t.elements.len(), d.layout.size() / 1024), 0);
            for e in &t.elements {
                let end = e.address as u64 + e.data.len() as u64;
                if e.address < d.layout.start || end > d.layout.start as u64 + d.layout.size() as u64 {
                    return Err(format!("The firmware block at 0x{:08X} does not fit into this processor's flash. Wrong DFU file?", e.address));
                }
            }
            for e in &t.elements {
                d.erase_range(e.address, e.data.len() as u32, &mut |i, n| prog(0.3 * i as f32 / n as f32, format!("Erasing page {i} of {n}")))?;
            }
            let mut done = 0usize;
            for e in &t.elements {
                let base = done;
                d.write(e.address, &e.data, &mut |n| prog(0.3 + 0.5 * (base + n) as f32 / total as f32, format!("Writing {}%", 100 * (base + n) / total)))?;
                done += e.data.len();
            }
            let mut checked = 0usize;
            for e in &t.elements {
                let base = checked;
                let back = d.read(e.address, e.data.len(), &mut |n| prog(0.8 + 0.2 * (base + n) as f32 / total as f32, format!("Verifying {}%", 100 * (base + n) / total)))?;
                if back.len() < e.data.len() || back[..e.data.len()] != e.data[..] {
                    let at = back.iter().zip(&e.data).position(|(a, b)| a != b).unwrap_or(back.len());
                    return Err(format!("Verify failed at 0x{:08X}. Please flash again.", e.address as usize + at));
                }
                checked += e.data.len();
            }
            prog(1.0, "Done".into());
            log("Download successful: the firmware was written and verified.".into(), 0);
            if leave {
                d.leave()?;
                log("Left DFU mode – the adapter restarts with the new firmware.".into(), 0);
            }
            Ok(())
        }
        FwTask::Backup(path) => {
            let dev = find_bootloader()?;
            let d = dfu::Dfu::open(&dev)?;
            let size = d.layout.size() as usize;
            let mut data = d.read(d.layout.start, size, &mut |n| prog(n as f32 / size as f32, format!("Reading {} KB", n / 1024)))?;
            while data.len() > 8 && data[data.len() - 1] == 0xFF {
                data.pop();
            }
            while data.len() % 8 != 0 {
                data.push(0xFF);
            }
            let dd = dev.device_descriptor();
            let mut f = dfufile::from_bin(data, d.layout.start);
            f.bcd_device = dd.device_version();
            let bytes = dfufile::build(&f);
            std::fs::write(&path, bytes).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
            prog(1.0, "Done".into());
            log(format!("Upload successful: {} KB saved to {}", f.total_bytes() / 1024, path.display()), 0);
            Ok(())
        }
        FwTask::Leave => {
            let dev = find_bootloader()?;
            let d = dfu::Dfu::open(&dev)?;
            d.leave()?;
            log("Successfully left DFU mode.".into(), 0);
            Ok(())
        }
        FwTask::DownloadLatest(dir) => download_latest(&dir, log, prog),
    }
}

const REPO_API: &str = "https://api.github.com/repos/Elmue/CANable-2.5-firmware-Slcan-and-Candlelight/contents/Binary%20Files";

fn download_latest(dir: &PathBuf, log: &dyn Fn(String, u8), prog: &dyn Fn(f32, String)) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    log("Asking GitHub for the latest CANable 2.5 firmware files…".into(), 0);
    let body = ureq::get(REPO_API)
        .header("User-Agent", "can-bench")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GitHub request failed: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let list: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let files: Vec<(String, String)> = list
        .as_array()
        .ok_or("Unexpected answer from GitHub")?
        .iter()
        .filter_map(|v| Some((v["name"].as_str()?.to_string(), v["download_url"].as_str()?.to_string())))
        .filter(|(n, _)| n.to_ascii_lowercase().ends_with(".dfu"))
        .collect();
    if files.is_empty() {
        return Err("GitHub returned no DFU files.".into());
    }
    let n = files.len();
    for (i, (name, url)) in files.iter().enumerate() {
        prog(i as f32 / n as f32, format!("Downloading {name}"));
        let bytes = ureq::get(url)
            .header("User-Agent", "can-bench")
            .call()
            .map_err(|e| format!("Download of {name} failed: {e}"))?
            .body_mut()
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        dfufile::parse(&bytes).map_err(|e| format!("{name}: {e}"))?;
        std::fs::write(dir.join(name), &bytes).map_err(|e| e.to_string())?;
    }
    prog(1.0, "Done".into());
    log(format!("{n} firmware files downloaded to {}", dir.display()), 0);
    Ok(())
}
