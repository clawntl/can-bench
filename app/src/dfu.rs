//! STM32 ROM bootloader in DFU mode (DfuSe extensions, ST AN3156).

use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use nusb::{Interface, MaybeFuture};
use std::time::Duration;

pub const VID: u16 = 0x0483;
pub const PID: u16 = 0xDF11;
const T: Duration = Duration::from_millis(5000);

pub const FLASH_START: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct Layout {
    pub name: String,
    pub start: u32,
    pub page_size: u32,
    pub pages: u32,
}

impl Layout {
    pub fn size(&self) -> u32 {
        self.page_size * self.pages
    }
}

/// "@Internal Flash  /0x08000000/64*02Kg" → start, pages, page size
pub fn parse_layout(s: &str) -> Option<Layout> {
    let mut parts = s.trim_start_matches('@').split('/');
    let name = parts.next()?.trim().to_string();
    let start = u32::from_str_radix(parts.next()?.trim().trim_start_matches("0x"), 16).ok()?;
    let mut pages = 0;
    let mut page_size = 0;
    for seg in parts.next()?.split(',') {
        let (n, rest) = seg.split_once('*')?;
        let n: u32 = n.trim().parse().ok()?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let unit = rest.chars().find(|c| c.is_ascii_alphabetic() && "KMB ".contains(*c)).unwrap_or('B');
        let mut sz: u32 = digits.parse().ok()?;
        sz *= match unit {
            'K' => 1024,
            'M' => 1024 * 1024,
            _ => 1,
        };
        if page_size == 0 {
            page_size = sz;
        }
        pages += n * sz / page_size.max(1);
    }
    Some(Layout { name, start, page_size, pages })
}

pub struct Dfu {
    pub intf: Interface,
    pub transfer: u16,
    pub layout: Layout,
}

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub status: u8,
    pub poll_ms: u32,
    pub state: u8,
}

pub fn status_text(s: u8) -> &'static str {
    match s {
        0 => "OK",
        1 => "file is not targeted for this device",
        2 => "file failed a verification test",
        3 => "device is unable to write memory",
        4 => "memory erase failed",
        5 => "memory erase check failed",
        6 => "program memory failed",
        7 => "programmed memory failed verification",
        8 => "address out of range",
        9 => "download incomplete",
        10 => "firmware is corrupt",
        11 => "vendor error (flash may be read protected)",
        12 => "unexpected USB reset",
        13 => "unexpected power on reset",
        14 => "unknown error",
        15 => "unexpected request",
        _ => "?",
    }
}

impl Dfu {
    pub fn open(dev: &nusb::Device) -> Result<Self, String> {
        let intf = dev.claim_interface(0).wait().map_err(|e| {
            format!(
                "Cannot open the STM32 bootloader ({e}). It needs the WinUSB driver: install it once for \"STM32 BOOTLOADER\" \
                 (for example with STM32CubeProgrammer's DFU driver or Zadig → WinUSB)."
            )
        })?;
        let _ = intf.set_alt_setting(0).wait();
        let mut layout = Layout { name: "Internal Flash".into(), start: FLASH_START, page_size: 2048, pages: 64 };
        let mut transfer = 2048u16;
        if let Some(d) = intf.descriptor() {
            for raw in d.descriptors() {
                // DFU functional descriptor: type 0x21, wTransferSize at offset 5
                if raw.descriptor_type() == 0x21 && raw.len() >= 7 {
                    let ts = u16::from_le_bytes([raw[5], raw[6]]);
                    if ts >= 64 {
                        transfer = ts;
                    }
                }
            }
            if let Some(si) = d.string_index() {
                if let Ok(s) = dev.get_string_descriptor(si, 0x0409, T).wait() {
                    if let Some(l) = parse_layout(&s) {
                        layout = l;
                    }
                }
            }
        }
        let me = Dfu { intf, transfer, layout };
        // bring the device into dfuIDLE
        if let Ok(s) = me.get_status() {
            if s.state == 10 {
                let _ = me.clr_status();
            } else if s.state != 2 {
                let _ = me.abort();
            }
        }
        Ok(me)
    }

    fn out(&self, req: u8, value: u16, data: &[u8]) -> Result<(), String> {
        self.intf
            .control_out(
                ControlOut { control_type: ControlType::Class, recipient: Recipient::Interface, request: req, value, index: 0, data },
                T,
            )
            .wait()
            .map_err(|e| format!("DFU request {req} failed: {e}"))
    }

    fn inp(&self, req: u8, value: u16, len: u16) -> Result<Vec<u8>, String> {
        self.intf
            .control_in(
                ControlIn { control_type: ControlType::Class, recipient: Recipient::Interface, request: req, value, index: 0, length: len },
                T,
            )
            .wait()
            .map_err(|e| format!("DFU request {req} failed: {e}"))
    }

    pub fn get_status(&self) -> Result<Status, String> {
        let s = self.inp(3, 0, 6)?;
        if s.len() < 6 {
            return Err("Short DFU status".into());
        }
        Ok(Status { status: s[0], poll_ms: u32::from_le_bytes([s[1], s[2], s[3], 0]), state: s[4] })
    }
    pub fn clr_status(&self) -> Result<(), String> {
        self.out(4, 0, &[])
    }
    pub fn abort(&self) -> Result<(), String> {
        self.out(6, 0, &[])
    }

    /// Polls GETSTATUS until the download has been processed.
    fn wait_done(&self) -> Result<Status, String> {
        for _ in 0..2000 {
            let s = self.get_status()?;
            if s.state == 10 || s.status != 0 {
                let _ = self.clr_status();
                return Err(format!("Bootloader error: {}", status_text(s.status)));
            }
            if s.state == 4 || s.state == 3 {
                std::thread::sleep(Duration::from_millis(s.poll_ms.clamp(1, 1000) as u64));
                continue;
            }
            return Ok(s);
        }
        Err("The bootloader stays busy".into())
    }

    fn command(&self, cmd: u8, addr: Option<u32>) -> Result<(), String> {
        let mut d = vec![cmd];
        if let Some(a) = addr {
            d.extend_from_slice(&a.to_le_bytes());
        }
        self.out(1, 0, &d)?;
        self.wait_done().map(|_| ())
    }

    pub fn set_address(&self, a: u32) -> Result<(), String> {
        self.command(0x21, Some(a))
    }
    pub fn erase_page(&self, a: u32) -> Result<(), String> {
        self.command(0x41, Some(a))
    }

    pub fn write(&self, addr: u32, data: &[u8], progress: &mut dyn FnMut(usize)) -> Result<(), String> {
        let ts = self.transfer as usize;
        let mut off = 0;
        while off < data.len() {
            let n = ts.min(data.len() - off);
            self.set_address(addr + off as u32)?;
            self.out(1, 2, &data[off..off + n])?;
            self.wait_done()?;
            off += n;
            progress(off);
        }
        Ok(())
    }

    pub fn read(&self, addr: u32, len: usize, progress: &mut dyn FnMut(usize)) -> Result<Vec<u8>, String> {
        let ts = self.transfer as usize;
        let mut out = Vec::with_capacity(len);
        let mut off = 0;
        while off < len {
            let n = ts.min(len - off);
            self.set_address(addr + off as u32)?;
            self.abort()?; // back to dfuIDLE before UPLOAD
            let chunk = self.inp(2, 2, n as u16)?;
            if chunk.is_empty() {
                break;
            }
            out.extend_from_slice(&chunk);
            off += chunk.len();
            progress(off);
        }
        let _ = self.abort();
        Ok(out)
    }

    /// Erases every page touched by [addr, addr+len).
    pub fn erase_range(&self, addr: u32, len: u32, progress: &mut dyn FnMut(u32, u32)) -> Result<(), String> {
        let ps = self.layout.page_size.max(256);
        let first = addr - (addr - self.layout.start) % ps;
        let mut a = first;
        let total = (addr + len - first).div_ceil(ps);
        let mut i = 0;
        while a < addr + len {
            self.erase_page(a)?;
            a += ps;
            i += 1;
            progress(i, total);
        }
        Ok(())
    }

    /// Jumps to the application (manifest). The device disconnects afterwards.
    pub fn leave(&self) -> Result<(), String> {
        self.set_address(FLASH_START)?;
        self.out(1, 0, &[])?;
        let _ = self.get_status();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn layout() {
        let l = super::parse_layout("@Internal Flash  /0x08000000/64*02Kg").unwrap();
        assert_eq!(l.start, 0x0800_0000);
        assert_eq!(l.page_size, 2048);
        assert_eq!(l.pages, 64);
        let l2 = super::parse_layout("@Internal Flash   /0x08000000/256*02Kg").unwrap();
        assert_eq!(l2.size(), 512 * 1024);
    }
}
