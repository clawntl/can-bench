//! ST DfuSe file format (UM0391): prefix, targets with elements, 16 byte suffix with CRC32.

#[derive(Clone, Debug)]
pub struct Element {
    pub address: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub alt_setting: u8,
    pub name: String,
    pub elements: Vec<Element>,
}

#[derive(Clone, Debug)]
pub struct DfuFile {
    pub targets: Vec<Target>,
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: u16,
}

impl DfuFile {
    pub fn total_bytes(&self) -> usize {
        self.targets.iter().flat_map(|t| &t.elements).map(|e| e.data.len()).sum()
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc // DfuSe stores it without the final inversion
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

pub fn parse(bytes: &[u8]) -> Result<DfuFile, String> {
    if bytes.len() < 11 + 16 {
        return Err("File too short".into());
    }
    let n = bytes.len();
    let suffix = &bytes[n - 16..];
    if &suffix[8..11] != b"UFD" || suffix[11] != 16 {
        return Err("Missing DFU suffix".into());
    }
    let crc_file = u32le(suffix, 12);
    if crc32(&bytes[..n - 4]) != crc_file {
        return Err("CRC error: the DFU file is corrupt".into());
    }
    let bcd_device = u16le(suffix, 0);
    let pid = u16le(suffix, 2);
    let vid = u16le(suffix, 4);
    if &bytes[0..5] != b"DfuSe" {
        return Err("Not a DfuSe file".into());
    }
    let n_targets = bytes[10] as usize;
    let mut pos = 11;
    let mut targets = Vec::new();
    for _ in 0..n_targets {
        if pos + 274 > n - 16 || &bytes[pos..pos + 6] != b"Target" {
            return Err("Bad target prefix".into());
        }
        let alt = bytes[pos + 6];
        let named = u32le(bytes, pos + 7) != 0;
        let name = if named {
            let raw = &bytes[pos + 11..pos + 11 + 255];
            let end = raw.iter().position(|&c| c == 0).unwrap_or(255);
            String::from_utf8_lossy(&raw[..end]).to_string()
        } else {
            String::new()
        };
        let n_el = u32le(bytes, pos + 270) as usize;
        pos += 274;
        let mut elements = Vec::new();
        for _ in 0..n_el {
            if pos + 8 > n - 16 {
                return Err("Truncated element".into());
            }
            let address = u32le(bytes, pos);
            let size = u32le(bytes, pos + 4) as usize;
            pos += 8;
            if pos + size > n - 16 {
                return Err("Truncated element data".into());
            }
            elements.push(Element { address, data: bytes[pos..pos + size].to_vec() });
            pos += size;
        }
        targets.push(Target { alt_setting: alt, name, elements });
    }
    Ok(DfuFile { targets, vid, pid, bcd_device })
}

pub fn build(file: &DfuFile) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"DfuSe");
    out.push(1);
    out.extend_from_slice(&[0; 4]); // image size, patched below
    out.push(file.targets.len() as u8);
    for t in &file.targets {
        out.extend_from_slice(b"Target");
        out.push(t.alt_setting);
        out.extend_from_slice(&1u32.to_le_bytes());
        let mut name = [0u8; 255];
        let nb = t.name.as_bytes();
        name[..nb.len().min(254)].copy_from_slice(&nb[..nb.len().min(254)]);
        out.extend_from_slice(&name);
        let size: usize = t.elements.iter().map(|e| 8 + e.data.len()).sum();
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&(t.elements.len() as u32).to_le_bytes());
        for e in &t.elements {
            out.extend_from_slice(&e.address.to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&e.data);
        }
    }
    let size = out.len() as u32;
    out[6..10].copy_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&file.bcd_device.to_le_bytes());
    out.extend_from_slice(&file.pid.to_le_bytes());
    out.extend_from_slice(&file.vid.to_le_bytes());
    out.extend_from_slice(&0x011Au16.to_le_bytes());
    out.extend_from_slice(b"UFD");
    out.push(16);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// A raw .bin image becomes a one-element DfuSe file at the flash start.
pub fn from_bin(bin: Vec<u8>, address: u32) -> DfuFile {
    DfuFile {
        targets: vec![Target {
            alt_setting: 0,
            name: "Internal Flash".into(),
            elements: vec![Element { address, data: bin }],
        }],
        vid: 0x0483,
        pid: 0xDF11,
        bcd_device: 0xFFFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let f = from_bin(vec![1, 2, 3, 4, 5], 0x0800_0000);
        let b = build(&f);
        let p = parse(&b).unwrap();
        assert_eq!(p.targets[0].elements[0].data, vec![1, 2, 3, 4, 5]);
        assert_eq!(p.targets[0].elements[0].address, 0x0800_0000);
    }

    #[test]
    fn real_files_parse() {
        let dir = std::path::Path::new(
            r"C:\Program Files (x86)\HUD ECU Hacker\Driver\CANable Firmware Update\Firmware",
        );
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let bytes = std::fs::read(e.path()).unwrap();
                let f = parse(&bytes).unwrap_or_else(|er| panic!("{:?}: {er}", e.path()));
                assert!(f.total_bytes() > 1000);
                assert_eq!(f.targets[0].elements[0].address, 0x0800_0000);
            }
        }
    }
}
