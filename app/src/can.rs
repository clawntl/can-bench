//! CAN frame model shared by both adapter types.

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanFrame {
    pub id: u32,
    pub ext: bool,
    pub rtr: bool,
    pub fd: bool,
    pub brs: bool,
    pub esi: bool,
    pub data: Vec<u8>,
}

impl CanFrame {
    pub fn flags_text(&self) -> String {
        let mut f = Vec::new();
        if self.rtr {
            f.push("RTR");
        }
        if self.fd {
            f.push("FDF");
        }
        if self.brs {
            f.push("BRS");
        }
        if self.esi {
            f.push("ESI");
        }
        if f.is_empty() {
            String::new()
        } else {
            format!("  ({})", f.join(", "))
        }
    }

    pub fn id_text(&self) -> String {
        if self.ext {
            format!("{:08X}", self.id)
        } else {
            format!("{:03X}", self.id)
        }
    }

    pub fn data_text(&self) -> String {
        self.data
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// "18EEFF01: 01 02 03  (FDF, BRS)" — the trace format of the original terminal.
    pub fn display(&self) -> String {
        if self.rtr {
            format!("{}: RTR DLC {}{}", self.id_text(), self.data.first().copied().unwrap_or(0), self.flags_text())
        } else {
            format!("{}: {}{}", self.id_text(), self.data_text(), self.flags_text())
        }
    }
}

/// CAN FD data lengths allowed per DLC code.
pub const FD_LENGTHS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];

pub fn len_to_dlc(len: usize) -> u8 {
    FD_LENGTHS.iter().position(|&l| l >= len).unwrap_or(15) as u8
}

pub fn dlc_to_len(dlc: u8) -> usize {
    FD_LENGTHS[(dlc & 0x0F) as usize]
}

/// Pads data up to the next valid CAN FD length (and to 8 bytes when asked).
pub fn pad(data: &mut Vec<u8>, fd: bool, always8: bool) {
    if always8 && data.len() < 8 {
        data.resize(8, 0);
    }
    if fd {
        let want = dlc_to_len(len_to_dlc(data.len()));
        data.resize(want, 0);
    }
}

/// Parses "11 22 33" / "112233" / "11,22" into bytes.
pub fn parse_hex_bytes(text: &str) -> Result<Vec<u8>, String> {
    let clean: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let other = text
        .chars()
        .any(|c| !(c.is_ascii_hexdigit() || c.is_whitespace() || c == ',' || c == '-' || c == ':'));
    if other {
        return Err("Data may only contain hex digits (0-9, A-F)".into());
    }
    // allow single digits separated by spaces ("1 2 3")
    let tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',' || c == '-' || c == ':')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.len() > 1 && tokens.iter().all(|t| t.len() <= 2) {
        return tokens
            .iter()
            .map(|t| u8::from_str_radix(t, 16).map_err(|e| e.to_string()))
            .collect();
    }
    if clean.len() % 2 != 0 {
        return Err("Odd number of hex digits".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

pub fn parse_hex_u32(text: &str) -> Option<u32> {
    let t = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    if t.is_empty() {
        return None;
    }
    u32::from_str_radix(t, 16).ok()
}

/// Host filter "id,mask" pairs separated by ';' (same syntax as the Slcan F command).
#[derive(Clone, Debug)]
pub struct HostFilter {
    pub id: u32,
    pub mask: u32,
    pub ext: bool,
}

pub fn parse_filters(text: &str) -> Result<Vec<HostFilter>, String> {
    let mut out = Vec::new();
    for part in text.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let mut it = part.split(',');
        let id_s = it.next().unwrap_or("").trim();
        let mask_s = it.next().unwrap_or("").trim();
        let id = parse_hex_u32(id_s).ok_or(format!("Bad filter ID '{id_s}'"))?;
        let ext = id_s.trim_start_matches("0x").len() > 3 || id > 0x7FF;
        let mask = if mask_s.is_empty() {
            if ext {
                0x1FFF_FFFF
            } else {
                0x7FF
            }
        } else {
            parse_hex_u32(mask_s).ok_or(format!("Bad filter mask '{mask_s}'"))?
        };
        out.push(HostFilter { id, mask, ext });
    }
    if out.len() > 8 {
        return Err("The adapter accepts at most 8 host filters".into());
    }
    Ok(out)
}
