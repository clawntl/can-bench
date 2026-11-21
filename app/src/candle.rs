//! Candlelight (gs_usb + ElmüSoft CANable 2.5 protocol) over WinUSB / libusb via nusb.

use crate::can::{CanFrame, HostFilter};
use crate::timing::{Limits, Timing};
use nusb::transfer::{Buffer, Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient};
use nusb::{Endpoint, Interface, MaybeFuture};
use std::time::Duration;

pub const IDS: [(u16, u16); 3] = [(0x1D50, 0x606F), (0x1209, 0x2323), (0x1CD2, 0x606F)];
pub const DFU_INTERFACE: u8 = 1;
const TIMEOUT: Duration = Duration::from_millis(1000);

// requests
pub const REQ_SET_BITTIMING: u8 = 1;
pub const REQ_SET_MODE: u8 = 2;
pub const REQ_GET_CAPS: u8 = 4;
pub const REQ_GET_VERSION: u8 = 5;
pub const REQ_IDENTIFY: u8 = 7;
pub const REQ_SET_BITTIMING_FD: u8 = 10;
pub const REQ_GET_CAPS_FD: u8 = 11;
pub const REQ_SET_TERMINATION: u8 = 12;
pub const REQ_GET_BOARD: u8 = 20;
pub const REQ_SET_FILTER: u8 = 21;
pub const REQ_GET_LAST_ERROR: u8 = 22;
pub const REQ_SET_BUSLOAD: u8 = 23;
pub const REQ_SET_PIN: u8 = 24;
pub const REQ_GET_PIN: u8 = 25;

// device flags
pub const F_LISTEN_ONLY: u32 = 0x1;
pub const F_LOOPBACK: u32 = 0x2;
pub const F_ONE_SHOT: u32 = 0x8;
pub const F_CAN_FD: u32 = 0x100;
pub const F_BITTIMING_FD: u32 = 0x400;
pub const F_TERMINATION: u32 = 0x800;
pub const F_ELMUE: u32 = 0x4000;
pub const F_BLOBS: u32 = 0x8000;
pub const F_IDENTIFY: u32 = 0x20;
pub const F_ERROR_REPORTS: u32 = 0x1000;

/// Firmware dates HUD ECU Hacker checks when opening (ECU_Hacker 0x06002144: 2490643, 2494465).
pub const FW_MINIMUM: u32 = 0x260113;
pub const FW_LATEST_KNOWN: u32 = 0x261001;
/// Firmware Tx queue that HUD counts against (ECU_Hacker 0x0600214e).
pub const TX_QUEUE: i32 = 64;

/// Free Tx slots from the last sent and last echoed markers (markers 1..=255).
pub fn free_tx_slots(tx: u8, rx_echo: u8) -> i32 {
    if tx == 0 {
        return TX_QUEUE;
    }
    let used = if tx < rx_echo { tx as i32 + 255 - rx_echo as i32 } else { tx as i32 - rx_echo as i32 };
    (TX_QUEUE - used).max(0)
}

// message types
const MSG_TX_FRAME: u8 = 10;
const MSG_TX_ECHO: u8 = 11;
const MSG_RX_FRAME: u8 = 12;
const MSG_ERROR: u8 = 13;
const MSG_STRING: u8 = 14;
const MSG_BUSLOAD: u8 = 15;
const MSG_RX_BLOB: u8 = 17;

pub fn feedback_text(code: u8) -> String {
    match code {
        b'1' => "The command is invalid.".into(),
        b'2' => "One of the parameters is invalid.".into(),
        b'3' => "This command needs the adapter to be open.".into(),
        b'4' => "This command needs the adapter to be closed.".into(),
        b'5' => "The ST HAL in the adapter reported an error.".into(),
        b'6' => "The adapter does not support this feature.".into(),
        b'7' => "Sending is not possible: the Tx buffer is full.".into(),
        b'8' => "Sending is not possible: the bus is off.".into(),
        b'9' => "Sending is not possible in silent (listen only) mode.".into(),
        b':' => "The baudrate has not been set.".into(),
        b';' => "Programming the option bytes failed.".into(),
        b'<' => "Please reconnect the USB cable (a hardware reset is required).".into(),
        b'=' => "A parameter is outside the valid range.".into(),
        c => format!("Unknown feedback code {c}"),
    }
}

#[derive(Clone, Debug, Default)]
pub struct CandleInfo {
    pub feature: u32,
    pub fclk: u32,
    pub nominal: Option<Limits>,
    pub data: Option<Limits>,
    pub sw_version: u32,
    pub hw_version: u32,
    pub hal: (u8, u8, u8),
    pub channels: u8,
    pub mcu_id: u16,
    pub mcu: String,
    pub board: String,
    pub quartz: bool,
    pub boot0_enabled: Option<bool>,
}

impl CandleInfo {
    pub fn elmue(&self) -> bool {
        self.feature & F_ELMUE != 0
    }
    pub fn fd(&self) -> bool {
        self.feature & F_CAN_FD != 0 && self.feature & F_BITTIMING_FD != 0
    }
}

pub fn bcd(v: u32) -> String {
    // firmware date 0x250914 → 25.09.14
    if v > 0xFFFF {
        format!("{:02X}.{:02X}.{:02X}", (v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF)
    } else {
        format!("{:X}.{:02X}", (v >> 8) & 0xFF, v & 0xFF)
    }
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn limits_at(b: &[u8], o: usize) -> Limits {
    Limits {
        seg1_max: le32(b, o + 4),
        seg2_max: le32(b, o + 12),
        sjw_max: le32(b, o + 16),
        brp_min: le32(b, o + 20).max(1),
        brp_max: le32(b, o + 24),
    }
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

pub fn ctrl_out(intf: &Interface, req: u8, value: u16, data: &[u8]) -> Result<(), String> {
    let r = intf
        .control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: req,
                value,
                index: intf.interface_number() as u16,
                data,
            },
            TIMEOUT,
        )
        .wait();
    match r {
        Ok(()) => Ok(()),
        Err(e) => Err(last_error(intf).unwrap_or_else(|| format!("USB request {req} failed: {e}"))),
    }
}

pub fn ctrl_in(intf: &Interface, req: u8, value: u16, len: u16) -> Result<Vec<u8>, String> {
    let r = intf
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: req,
                value,
                index: intf.interface_number() as u16,
                length: len,
            },
            TIMEOUT,
        )
        .wait();
    match r {
        Ok(v) => Ok(v),
        Err(e) => Err(last_error(intf).unwrap_or_else(|| format!("USB request {req} failed: {e}"))),
    }
}

/// ELM_ReqGetLastError: the feedback code of the last failed request.
fn last_error(intf: &Interface) -> Option<String> {
    let v = intf
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: REQ_GET_LAST_ERROR,
                value: 0,
                index: intf.interface_number() as u16,
                length: 1,
            },
            TIMEOUT,
        )
        .wait()
        .ok()?;
    let c = *v.first()?;
    if c == 2 {
        None
    } else {
        Some(feedback_text(c))
    }
}

pub fn set_mode(intf: &Interface, ch: u8, mode: u32, flags: u32) -> Result<(), String> {
    let mut d = Vec::with_capacity(8);
    d.extend_from_slice(&mode.to_le_bytes());
    d.extend_from_slice(&flags.to_le_bytes());
    ctrl_out(intf, REQ_SET_MODE, ch as u16, &d)
}

pub fn read_info(intf: &Interface, ch: u8) -> Result<CandleInfo, String> {
    // Reset first: closes the channel and resets all firmware variables.
    set_mode(intf, ch, 0, F_ELMUE)?;
    let caps = ctrl_in(intf, REQ_GET_CAPS, ch as u16, 40)?;
    if caps.len() < 40 {
        return Err("The adapter returned short capabilities".into());
    }
    let mut info = CandleInfo {
        feature: le32(&caps, 0),
        fclk: le32(&caps, 4),
        nominal: Some(limits_at(&caps, 8)),
        ..Default::default()
    };
    if info.fd() {
        if let Ok(fd) = ctrl_in(intf, REQ_GET_CAPS_FD, ch as u16, 72) {
            if fd.len() >= 72 {
                info.nominal = Some(limits_at(&fd, 8));
                info.data = Some(limits_at(&fd, 40));
            }
        }
    }
    let ver = ctrl_in(intf, REQ_GET_VERSION, ch as u16, 12)?;
    if ver.len() >= 12 {
        info.hal = (ver[0], ver[1], ver[2]);
        info.channels = ver[3] + 1;
        info.sw_version = le32(&ver, 4);
        info.hw_version = le32(&ver, 8);
    }
    if info.elmue() {
        if let Ok(b) = ctrl_in(intf, REQ_GET_BOARD, ch as u16, 56) {
            if b.len() >= 52 {
                info.mcu_id = u16::from_le_bytes([b[0], b[1]]);
                info.mcu = cstr(&b[2..27]);
                info.board = cstr(&b[27..52]);
                if b.len() >= 56 {
                    info.quartz = le32(&b, 52) & 1 != 0;
                }
            }
        }
        if let Ok(p) = ctrl_in(intf, REQ_GET_PIN, 1, 2) {
            if p.len() >= 2 {
                info.boot0_enabled = Some(u16::from_le_bytes([p[0], p[1]]) & 2 != 0);
            }
        }
    }
    Ok(info)
}

pub fn info_lines(info: &CandleInfo, ch: u8) -> Vec<String> {
    let mut v = Vec::new();
    v.push(format!(
        "Firmware: {}, Hardware: {}, Type: {}",
        bcd(info.sw_version),
        bcd(info.hw_version),
        if info.elmue() { "CANable 2.5 (ElmüSoft)" } else { "Legacy Candlelight" }
    ));
    if info.elmue() {
        v.push(format!(
            "Board: {}, Processor: {}, Clock: {} MHz, Device ID: 0x{:X}, Quartz: {}",
            info.board,
            info.mcu,
            info.fclk / 1_000_000,
            info.mcu_id,
            if info.quartz { "Yes" } else { "No" }
        ));
        v.push(format!(
            "HAL {}.{}.{}, CAN channel {} of {}, Pin BOOT0: {}",
            info.hal.0,
            info.hal.1,
            info.hal.2,
            ch + 1,
            info.channels,
            match info.boot0_enabled {
                Some(true) => "enabled",
                Some(false) => "disabled",
                None => "?",
            }
        ));
    } else {
        v.push(format!("CAN clock: {} MHz", info.fclk / 1_000_000));
    }
    v.push(format!("Supports CAN FD: {}", if info.fd() { "Yes" } else { "No" }));
    v
}

pub fn set_timing(intf: &Interface, ch: u8, fd: bool, t: &Timing) -> Result<(), String> {
    let mut d = Vec::with_capacity(20);
    for v in [0, t.seg1, t.seg2, t.sjw, t.brp] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    ctrl_out(intf, if fd { REQ_SET_BITTIMING_FD } else { REQ_SET_BITTIMING }, ch as u16, &d)
}

pub fn add_filter(intf: &Interface, ch: u8, f: &HostFilter) -> Result<(), String> {
    let mut d = vec![if f.ext { 2u8 } else { 1u8 }];
    d.extend_from_slice(&f.id.to_le_bytes());
    d.extend_from_slice(&f.mask.to_le_bytes());
    d.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // index, dest channel, reserved[6]
    ctrl_out(intf, REQ_SET_FILTER, ch as u16, &d)
}

pub fn identify(intf: &Interface, ch: u8, on: bool) -> Result<(), String> {
    ctrl_out(intf, REQ_IDENTIFY, ch as u16, &(on as u32).to_le_bytes())
}

pub fn disable_boot0(intf: &Interface, ch: u8) -> Result<(), String> {
    set_mode(intf, ch, 0, F_ELMUE)?;
    let caps = ctrl_in(intf, REQ_GET_CAPS, ch as u16, 40)?;
    if caps.len() < 4 || le32(&caps, 0) & F_ELMUE == 0 {
        return Err("This feature needs the CANable 2.5 firmware from ElmüSoft.".into());
    }
    let mut d = Vec::new();
    d.extend_from_slice(&5u16.to_le_bytes()); // PINOP_Disable
    d.extend_from_slice(&1u16.to_le_bytes()); // PINID_BOOT0
    d.extend_from_slice(&[0; 8]);
    ctrl_out(intf, REQ_SET_PIN, ch as u16, &d)
}

/// DFU_DETACH on the run-time DFU interface (1), then DFU_GETSTATUS.
/// Ok(true) = adapter will enter boot mode in 300 ms; Ok(false) = user must reconnect the USB cable.
pub fn enter_dfu(dfu_intf: &Interface) -> Result<bool, String> {
    let idx = dfu_intf.interface_number() as u16;
    dfu_intf
        .control_out(
            ControlOut { control_type: ControlType::Class, recipient: Recipient::Interface, request: 0, value: 0, index: idx, data: &[] },
            TIMEOUT,
        )
        .wait()
        .map_err(|e| format!("DFU detach failed: {e}"))?;
    let st = dfu_intf
        .control_in(
            ControlIn { control_type: ControlType::Class, recipient: Recipient::Interface, request: 3, value: 0, index: idx, length: 6 },
            TIMEOUT,
        )
        .wait();
    if let Ok(s) = st {
        if s.len() >= 6 {
            if s[4] == 1 {
                return Ok(false);
            }
            if s[4] == 10 && s[5] > 0 && s[5] < 255 {
                return Err(feedback_text(s[5]));
            }
        }
    }
    Ok(true)
}

pub fn tx_bytes(f: &CanFrame, marker: u8) -> Vec<u8> {
    let mut id = f.id & if f.ext { 0x1FFF_FFFF } else { 0x7FF };
    if f.ext {
        id |= 0x8000_0000;
    }
    if f.rtr {
        id |= 0x4000_0000;
    }
    let mut flags = 0u8;
    if f.fd {
        flags |= 0x02;
    }
    if f.brs {
        flags |= 0x04;
    }
    let mut v = Vec::with_capacity(8 + f.data.len());
    v.push((8 + f.data.len()) as u8);
    v.push(MSG_TX_FRAME);
    v.push(flags);
    v.extend_from_slice(&id.to_le_bytes());
    v.push(marker);
    v.extend_from_slice(&f.data);
    v
}

#[derive(Debug)]
pub enum InMsg {
    Rx(CanFrame),
    Echo(u8),
    Error { err_id: u32, data: [u8; 8] },
    Text(String),
    Busload(u8),
}

/// Splits one USB IN transfer (single message or blob) into messages.
pub fn parse_in(buf: &[u8]) -> Vec<InMsg> {
    let mut out = Vec::new();
    let mut off = 0;
    if buf.len() >= 2 && buf[1] == MSG_RX_BLOB {
        off = 2;
    }
    while off + 2 <= buf.len() {
        let size = buf[off] as usize;
        if size < 2 || off + size > buf.len() {
            break;
        }
        let m = &buf[off..off + size];
        match m[1] {
            MSG_RX_FRAME if size >= 7 => {
                let flags = m[2];
                let raw = le32(m, 3);
                let fd = flags & 0x02 != 0;
                out.push(InMsg::Rx(CanFrame {
                    id: raw & 0x1FFF_FFFF,
                    ext: raw & 0x8000_0000 != 0,
                    rtr: raw & 0x4000_0000 != 0,
                    fd,
                    brs: fd && flags & 0x04 != 0,
                    esi: fd && flags & 0x08 != 0,
                    data: m[7..].to_vec(),
                }));
            }
            MSG_TX_ECHO if size >= 3 => out.push(InMsg::Echo(m[2])),
            MSG_ERROR if size >= 14 => {
                let mut d = [0u8; 8];
                d.copy_from_slice(&m[6..14]);
                out.push(InMsg::Error { err_id: le32(m, 2), data: d });
            }
            MSG_STRING => out.push(InMsg::Text(String::from_utf8_lossy(&m[2..]).trim_end_matches('\0').to_string())),
            MSG_BUSLOAD if size >= 3 => out.push(InMsg::Busload(m[2])),
            _ => {}
        }
        off += size;
    }
    out
}

/// Returns (text, severity 0 low / 1 medium / 2 high).
pub fn error_text(err_id: u32, d: &[u8; 8]) -> (String, u8) {
    let mut s = Vec::<String>::new();
    let mut level = 0;
    if err_id & 0x40 != 0 {
        s.push("Bus Off".into());
        level = 2;
    } else if d[1] & 0x30 != 0 {
        s.push("Bus Passive".into());
        level = 2;
    } else if d[1] & 0x0C != 0 {
        s.push("Bus Warning".into());
        level = 1;
    } else if d[1] & 0x40 != 0 {
        s.push("Back to Active".into());
    } else {
        s.push("Bus Active".into());
    }
    let app = d[5];
    if app != 0 {
        level = 2;
    }
    for (bit, t) in [(0x01, "Rx Failed"), (0x02, "Tx Failed"), (0x10, "Tx Timeout"), (0x04, "CAN Tx Overflow"), (0x08, "USB IN Overflow")] {
        if app & bit != 0 {
            s.push(t.into());
        }
    }
    if err_id & 0x20 != 0 {
        s.push("No ACK received".into());
    }
    if err_id & 0x200 != 0 {
        s.push("CRC Error".into());
    }
    for (bit, t) in [(0x04, "Bit Stuffing Error"), (0x02, "Frame Format Error"), (0x08, "Dominant Bit Error"), (0x10, "Recessive Bit Error")] {
        if d[2] & bit != 0 {
            s.push(t.into());
        }
    }
    if d[6] > 0 {
        s.push(format!("Tx Errors: {}", d[6]));
    }
    if d[7] > 0 {
        s.push(format!("Rx Errors: {}", d[7]));
    }
    (s.join(", "), level)
}

/// An open, started Candlelight channel.
pub struct Candle {
    pub intf: Interface,
    #[allow(dead_code)]
    pub ch: u8,
    pub ep_in: Endpoint<Bulk, In>,
    pub ep_out: Endpoint<Bulk, Out>,
}

impl Candle {
    pub fn open(intf: Interface, ch: u8) -> Result<Self, String> {
        let desc = intf.descriptor().ok_or("No interface descriptor")?;
        let mut a_in = None;
        let mut a_out = None;
        for ep in desc.endpoints() {
            if ep.address() & 0x80 != 0 {
                a_in = Some(ep.address());
            } else {
                a_out = Some(ep.address());
            }
        }
        let ep_in = intf
            .endpoint::<Bulk, In>(a_in.ok_or("No IN endpoint")?)
            .map_err(|e| e.to_string())?;
        let ep_out = intf
            .endpoint::<Bulk, Out>(a_out.ok_or("No OUT endpoint")?)
            .map_err(|e| e.to_string())?;
        Ok(Candle { intf, ch, ep_in, ep_out })
    }

    pub fn start_reading(&mut self) {
        while self.ep_in.pending() < 4 {
            self.ep_in.submit(Buffer::new(2048));
        }
    }

    /// Waits up to `t` for one IN transfer.
    pub fn poll(&mut self, t: Duration) -> Result<Option<Vec<u8>>, String> {
        self.start_reading();
        match self.ep_in.wait_next_complete(t) {
            None => Ok(None),
            Some(c) => {
                let res = c.status;
                let data = c.buffer.into_vec();
                self.ep_in.submit(Buffer::new(2048));
                match res {
                    Ok(()) => Ok(Some(data)),
                    Err(e) => Err(format!("USB read failed: {e}")),
                }
            }
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut b = Buffer::new(bytes.len());
        b.extend_from_slice(bytes);
        let c = self.ep_out.transfer_blocking(b, Duration::from_millis(500));
        c.status.map_err(|e| format!("USB write failed: {e}"))
    }
}
