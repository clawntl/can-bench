//! Slcan (ASCII over a USB COM port), CANable 2.5 dialect with legacy fallback.

use crate::can::{dlc_to_len, len_to_dlc, CanFrame};
use crate::timing::Limits;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub const IDS: [(u16, u16); 2] = [(0x16D0, 0x117E), (0xAD50, 0x60C4)];

#[derive(Clone, Debug, Default)]
pub struct SlcanInfo {
    pub elmue: bool,
    pub board: String,
    pub mcu: String,
    pub dev_id: u32,
    pub firmware: u32,
    pub slcan_ver: u32,
    pub clock_mhz: u32,
    pub channels: u32,
    pub quartz: bool,
    pub nominal: Option<Limits>,
    pub data: Option<Limits>,
    pub serial: String,
    pub raw: String,
}

pub fn parse_version(line: &str) -> SlcanInfo {
    let body = line.trim_start_matches('+');
    let parts: Vec<&str> = body.split('\t').collect();
    let mut i = SlcanInfo { raw: body.to_string(), ..Default::default() };
    if parts.len() < 7 {
        return i;
    }
    i.elmue = true;
    for p in parts {
        let mut kv = p.splitn(2, ':');
        let k = kv.next().unwrap_or("").trim();
        let v = kv.next().unwrap_or("").trim();
        match k {
            "Board" => i.board = v.into(),
            "MCU" => i.mcu = v.into(),
            "DevID" => i.dev_id = v.parse().unwrap_or(0),
            "Firmware" => i.firmware = v.parse().unwrap_or(0),
            "Slcan" => i.slcan_ver = v.parse().unwrap_or(0),
            "Clock" => i.clock_mhz = v.parse().unwrap_or(0),
            "Channels" => i.channels = v.parse().unwrap_or(1),
            "Quartz" => i.quartz = v.eq_ignore_ascii_case("yes"),
            "Serial" => i.serial = v.into(),
            "Limits" => {
                let n: Vec<u32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if n.len() >= 8 {
                    i.nominal = Some(Limits { brp_min: 1, brp_max: n[0], seg1_max: n[1], seg2_max: n[2], sjw_max: n[3] });
                    i.data = Some(Limits { brp_min: 1, brp_max: n[4], seg1_max: n[5], seg2_max: n[6], sjw_max: n[7] });
                }
            }
            _ => {}
        }
    }
    i
}

pub fn info_lines(i: &SlcanInfo) -> Vec<String> {
    if !i.elmue {
        return vec![format!("Legacy Slcan firmware: {}", i.raw)];
    }
    vec![
        format!("Board: {}, Processor: {}, Clock: {} MHz, Device ID: 0x{:X}, Quartz: {}", i.board, i.mcu, i.clock_mhz, i.dev_id, if i.quartz { "Yes" } else { "No" }),
        format!("Firmware: {:06X}, Slcan version {}, Channels: {}, Serial: {}", i.firmware, i.slcan_ver, i.channels, i.serial),
    ]
}

/// "Exxxxxxxx" error report → (text, severity)
pub fn error_text(code: &str) -> (String, u8) {
    let b = code.as_bytes();
    if b.len() < 8 {
        return (format!("Error report {code}"), 2);
    }
    let mut s = Vec::<String>::new();
    let level = match b[0] {
        b'0' => {
            s.push("Bus Active".into());
            0
        }
        b'1' => {
            s.push("Bus Warning".into());
            1
        }
        b'2' => {
            s.push("Bus Passive".into());
            2
        }
        _ => {
            s.push("Bus Off".into());
            2
        }
    };
    let proto = ["", "Bit Stuffing Error", "Frame Format Error", "No ACK received", "Recessive Bit Error", "Dominant Bit Error", "CRC Error"];
    let p = (b[1] - b'0') as usize;
    if p > 0 && p < proto.len() {
        s.push(proto[p].into());
    }
    let hex = |o: usize| u8::from_str_radix(&code[o..o + 2], 16).unwrap_or(0);
    let app = hex(2);
    for (bit, t) in [(0x01, "Rx Failed"), (0x02, "Tx Failed"), (0x04, "CAN Tx Overflow"), (0x08, "USB IN Overflow"), (0x10, "Tx Timeout")] {
        if app & bit != 0 {
            s.push(t.into());
        }
    }
    let (tx, rx) = (hex(4), hex(6));
    if tx > 0 {
        s.push(format!("Tx Errors: {tx}"));
    }
    if rx > 0 {
        s.push(format!("Rx Errors: {rx}"));
    }
    (s.join(", "), if app != 0 { 2 } else { level })
}

pub fn frame_cmd(f: &CanFrame, marker: Option<u8>) -> String {
    let c = match (f.fd, f.brs, f.rtr, f.ext) {
        (true, true, _, false) => 'b',
        (true, true, _, true) => 'B',
        (true, false, _, false) => 'd',
        (true, false, _, true) => 'D',
        (false, _, true, false) => 'r',
        (false, _, true, true) => 'R',
        (false, _, false, false) => 't',
        (false, _, false, true) => 'T',
    };
    let mut s = String::new();
    s.push(c);
    s += &f.id_text();
    if f.rtr {
        s += &format!("{:X}", f.data.first().copied().unwrap_or(0) & 0xF);
    } else {
        s += &format!("{:X}", len_to_dlc(f.data.len()));
        for b in &f.data {
            s += &format!("{b:02X}");
        }
    }
    if let Some(m) = marker {
        s += &format!("{m:02X}");
    }
    s
}

pub fn parse_frame(line: &str) -> Option<CanFrame> {
    let mut chars = line.chars();
    let c = chars.next()?;
    let (ext, rtr, fd, brs) = match c {
        't' => (false, false, false, false),
        'T' => (true, false, false, false),
        'r' => (false, true, false, false),
        'R' => (true, true, false, false),
        'd' => (false, false, true, false),
        'D' => (true, false, true, false),
        'b' => (false, false, true, true),
        'B' => (true, false, true, true),
        _ => return None,
    };
    let rest = &line[1..];
    let idl = if ext { 8 } else { 3 };
    if rest.len() < idl + 1 {
        return None;
    }
    let id = u32::from_str_radix(&rest[..idl], 16).ok()?;
    let dlc = u8::from_str_radix(&rest[idl..idl + 1], 16).ok()?;
    let mut data = Vec::new();
    let mut esi = false;
    if rtr {
        data.push(dlc);
    } else {
        let n = dlc_to_len(dlc);
        let hex = &rest[idl + 1..];
        if hex.len() < n * 2 {
            return None;
        }
        for i in 0..n {
            data.push(u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?);
        }
        esi = fd && hex[n * 2..].starts_with('S');
    }
    Some(CanFrame { id, ext, rtr, fd, brs, esi, data })
}

pub struct Slcan {
    pub port: Box<dyn serialport::SerialPort>,
    buf: Vec<u8>,
    pub pending: Vec<String>,
    /// legacy firmware answers CR / BEL instead of "#x"
    pub feedback: bool,
}

impl Slcan {
    pub fn open(name: &str) -> Result<Self, String> {
        let port = serialport::new(name, 115_200)
            .timeout(Duration::from_millis(5))
            .open()
            .map_err(|e| format!("Cannot open {name}: {e}"))?;
        Ok(Slcan { port, buf: Vec::new(), pending: Vec::new(), feedback: false })
    }

    pub fn send_raw(&mut self, s: &str) -> Result<(), String> {
        self.port.write_all(s.as_bytes()).map_err(|e| format!("Write failed: {e}"))?;
        let _ = self.port.flush();
        Ok(())
    }

    /// Reads whatever arrived; complete lines go to `pending`. BEL is kept as "\x07".
    pub fn pump(&mut self) -> Result<(), String> {
        let mut tmp = [0u8; 4096];
        match self.port.read(&mut tmp) {
            Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(format!("Read failed: {e}")),
        }
        while let Some(p) = self.buf.iter().position(|&c| c == b'\r' || c == 0x07) {
            let end = self.buf[p];
            let line: Vec<u8> = self.buf.drain(..=p).collect();
            let mut s = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
            if end == 0x07 {
                s.push('\x07');
            }
            self.pending.push(s);
        }
        Ok(())
    }

    /// Sends a command and waits for its feedback; other lines stay in `pending` (returned).
    pub fn command(&mut self, cmd: &str, other: &mut Vec<String>) -> Result<String, String> {
        self.send_raw(&format!("{cmd}\r"))?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1500) {
            self.pump()?;
            let lines: Vec<String> = self.pending.drain(..).collect();
            let mut answer = None;
            for l in lines {
                if answer.is_some() {
                    other.push(l);
                    continue;
                }
                let t = l.trim_start_matches(['&', '$']);
                if self.feedback && t.starts_with('#') {
                    if t.len() == 1 {
                        answer = Some(Ok(String::new()));
                    } else {
                        answer = Some(Err(crate::candle::feedback_text(t.as_bytes()[1])));
                    }
                } else if t.starts_with('+') {
                    answer = Some(Ok(t.to_string()));
                } else if !self.feedback && t.ends_with('\x07') {
                    answer = Some(Err(format!("The adapter refused '{cmd}'")));
                } else if !self.feedback && t.is_empty() {
                    answer = Some(Ok(String::new()));
                } else if !self.feedback && t.starts_with('V') {
                    answer = Some(Ok(t.to_string()));
                } else {
                    other.push(l);
                }
            }
            if let Some(a) = answer {
                // "+text" answers are followed by a feedback "#" in feedback mode
                if let Ok(ref s) = a {
                    if s.starts_with('+') && self.feedback {
                        let t0 = Instant::now();
                        while t0.elapsed() < Duration::from_millis(300) {
                            self.pump()?;
                            if let Some(i) = self.pending.iter().position(|l| l.starts_with('#')) {
                                self.pending.remove(i);
                                break;
                            }
                        }
                    }
                }
                return a;
            }
        }
        Err(format!("No answer from the adapter to '{cmd}'"))
    }

    /// Close + read version. Returns the parsed info.
    pub fn init(&mut self) -> Result<SlcanInfo, String> {
        self.send_raw("\rC\r")?;
        std::thread::sleep(Duration::from_millis(100));
        let _ = self.pump();
        self.pending.clear();
        self.feedback = false;
        let mut other = Vec::new();
        let v = self.command("V", &mut other)?;
        let info = parse_version(&v);
        if info.elmue {
            self.feedback = true;
            self.send_raw("MF\r")?;
            let start = Instant::now();
            loop {
                self.pump()?;
                if let Some(i) = self.pending.iter().position(|l| l.starts_with('#')) {
                    self.pending.remove(i);
                    break;
                }
                if start.elapsed() > Duration::from_millis(1000) {
                    return Err("The adapter did not confirm feedback mode".into());
                }
            }
        }
        Ok(info)
    }
}

/// Legacy "Sx" codes for firmware without the s/y commands.
pub fn legacy_s(baud: u32) -> Option<char> {
    let table = [(10_000, '0'), (20_000, '1'), (50_000, '2'), (100_000, '3'), (125_000, '4'), (250_000, '5'), (500_000, '6'), (800_000, '7'), (1_000_000, '8'), (83_333, '9')];
    table.iter().find(|(b, _)| *b == baud).map(|(_, c)| *c)
}

pub fn legacy_y(baud: u32) -> Option<char> {
    let table = [(500_000, '0'), (1_000_000, '1'), (2_000_000, '2'), (4_000_000, '4'), (5_000_000, '5'), (8_000_000, '8')];
    table.iter().find(|(b, _)| *b == baud).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames() {
        let f = parse_frame("T18AABBCC51122334455").unwrap();
        assert_eq!(f.id, 0x18AABBCC);
        assert_eq!(f.data, vec![0x11, 0x22, 0x33, 0x44, 0x55]);
        assert_eq!(frame_cmd(&f, None), "T18AABBCC51122334455");
        let g = parse_frame("b7E081122334455667788S").unwrap();
        assert!(g.esi && g.brs && g.fd);
        let h = parse_frame("d7E09112233445566778899AABBCC").unwrap();
        assert_eq!(h.data.len(), 12);
    }
    #[test]
    fn errors() {
        let (t, _) = error_text("2310800C");
        assert!(t.contains("Bus Passive") && t.contains("No ACK") && t.contains("Tx Timeout") && t.contains("128") && t.contains("12"));
    }
}
