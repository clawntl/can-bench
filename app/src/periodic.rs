//! Periodic transmit: the frame list edited in the window, and the scheduler that runs
//! inside the adapter thread (so timing does not depend on the window being repainted,
//! e.g. while it is minimised).

use crate::can::{self, CanFrame};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// What the user typed for one frame (Send bar or a periodic row).
#[derive(Clone, Debug, PartialEq)]
pub struct TxSpec {
    pub id: String,
    pub data: String,
    pub ext: bool,
    pub rtr: bool,
    pub fd: bool,
    pub brs: bool,
}

impl Default for TxSpec {
    fn default() -> Self {
        TxSpec { id: "123".into(), data: "11 22 33 44 55 66 77 88".into(), ext: false, rtr: false, fd: false, brs: false }
    }
}

impl TxSpec {
    /// `link_fd`: the adapter was opened with CAN FD. `pad8`: pad classic frames to 8 bytes.
    pub fn build(&self, link_fd: bool, pad8: bool) -> Result<CanFrame, String> {
        let id = can::parse_hex_u32(&self.id).ok_or("Enter the CAN ID in hex, e.g. 123 or 18DAF110")?;
        let ext = self.ext || id > 0x7FF || self.id.trim().len() > 3;
        if (!ext && id > 0x7FF) || id > 0x1FFF_FFFF {
            return Err("The CAN ID is too big".into());
        }
        let fd = self.fd;
        if fd && !link_fd {
            return Err("CAN FD packets need 'CAN FD' to be ticked before opening.".into());
        }
        let mut data = if self.rtr { Vec::new() } else { can::parse_hex_bytes(&self.data)? };
        if self.rtr {
            if fd {
                return Err("Remote frames do not exist in CAN FD".into());
            }
            let n = can::parse_hex_bytes(&self.data).map(|v| v.len()).unwrap_or(0).min(8);
            data.push(n as u8);
        } else {
            if data.len() > if fd { 64 } else { 8 } {
                return Err(format!("Too many data bytes ({}), max {}", data.len(), if fd { 64 } else { 8 }));
            }
            can::pad(&mut data, fd, pad8);
        }
        Ok(CanFrame { id, ext, rtr: self.rtr, fd, brs: fd && self.brs, esi: false, data })
    }
}

/// One row of the periodic list.
#[derive(Clone, Debug)]
pub struct Entry {
    pub on: bool,
    pub name: String,
    pub spec: TxSpec,
    pub period_ms: u32,
    /// frames sent since the list was last started (not saved)
    pub sent: u64,
    /// why this row cannot be sent (not saved)
    pub err: Option<String>,
}

impl Entry {
    pub fn new(spec: TxSpec) -> Self {
        Entry { on: false, name: String::new(), spec, period_ms: 100, sent: 0, err: None }
    }
}

pub fn load(path: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let b = |v: &Value, k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    rows.iter()
        .map(|v| Entry {
            // never start transmitting by itself after a restart
            on: false,
            name: s(v, "name"),
            spec: TxSpec { id: s(v, "id"), data: s(v, "data"), ext: b(v, "ext"), rtr: b(v, "rtr"), fd: b(v, "fd"), brs: b(v, "brs") },
            period_ms: v.get("period_ms").and_then(Value::as_u64).unwrap_or(100).clamp(1, 60_000) as u32,
            sent: 0,
            err: None,
        })
        .collect()
}

pub fn save(path: &Path, list: &[Entry]) -> std::io::Result<()> {
    let rows: Vec<Value> = list
        .iter()
        .map(|e| {
            json!({
                "name": e.name, "id": e.spec.id, "data": e.spec.data, "ext": e.spec.ext,
                "rtr": e.spec.rtr, "fd": e.spec.fd, "brs": e.spec.brs, "period_ms": e.period_ms,
            })
        })
        .collect();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&rows).unwrap_or_default())
}

/// A frame to send every `period`; `tag` comes back with each Tx event (the row index).
pub struct Job {
    pub tag: usize,
    pub frame: CanFrame,
    pub period: Duration,
}

/// Runs in the adapter thread.
#[derive(Default)]
pub struct Scheduler {
    jobs: Vec<(Job, Instant)>,
}

impl Scheduler {
    /// Replaces the job list. A job that already ran (same tag, CAN ID and period) keeps its phase,
    /// so changing its data (a slider) or editing another row does not disturb the timing.
    pub fn set(&mut self, jobs: Vec<Job>, now: Instant) {
        let old = std::mem::take(&mut self.jobs);
        for j in jobs {
            let next = old.iter().find(|(o, _)| o.tag == j.tag && o.frame.id == j.frame.id && o.period == j.period).map(|(_, t)| *t).unwrap_or(now);
            self.jobs.push((j, next));
        }
    }

    /// Frames that are due now, with their tag.
    pub fn due(&mut self, now: Instant) -> Vec<(CanFrame, Option<usize>)> {
        let mut out = Vec::new();
        for (j, next) in &mut self.jobs {
            if now >= *next {
                out.push((j.frame.clone(), Some(j.tag)));
                *next += j.period;
                // fell behind (thread stalled, USB busy): skip the missed slots instead of bursting
                if *next <= now {
                    *next = now + j.period;
                }
            }
        }
        out
    }

    /// Time until the next job is due (None = no jobs).
    pub fn next_in(&self, now: Instant) -> Option<Duration> {
        self.jobs.iter().map(|(_, t)| t.saturating_duration_since(now)).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_classic_and_extended() {
        let f = TxSpec { id: "010".into(), data: "00 00 00 00".into(), ..Default::default() }.build(false, true).unwrap();
        assert_eq!((f.id, f.ext, f.data.len()), (0x10, false, 8));
        let f = TxSpec { id: "18DAF110".into(), data: "01".into(), ..Default::default() }.build(false, false).unwrap();
        assert!(f.ext);
        assert!(TxSpec { fd: true, ..Default::default() }.build(false, false).is_err());
    }

    #[test]
    fn schedules_without_bursting() {
        let t0 = Instant::now();
        let f = TxSpec::default().build(false, false).unwrap();
        let mut s = Scheduler::default();
        s.set(vec![Job { tag: 3, frame: f.clone(), period: Duration::from_millis(10) }], t0);
        assert_eq!(s.due(t0).len(), 1);
        assert_eq!(s.due(t0 + Duration::from_millis(5)).len(), 0);
        assert_eq!(s.due(t0 + Duration::from_millis(10))[0].1, Some(3));
        // a 100 ms stall gives one frame, not ten
        assert_eq!(s.due(t0 + Duration::from_millis(110)).len(), 1);
        assert_eq!(s.next_in(t0 + Duration::from_millis(110)), Some(Duration::from_millis(10)));
        // same job re-set keeps its phase
        s.set(vec![Job { tag: 3, frame: f, period: Duration::from_millis(10) }], t0 + Duration::from_millis(111));
        assert_eq!(s.due(t0 + Duration::from_millis(111)).len(), 0);
    }
}
