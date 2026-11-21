//! Demo bus (`--demo`): a virtual adapter with two simulated BLDC-Driver nodes
//! (github.com/clawntl/BLDC-Driver, CAN protocol 4), so the Device page and
//! profiles/bldc-driver.toml can be tried — and screenshotted — without hardware.
//! The motor model is deliberately simple: it only has to look plausible.

use crate::can::CanFrame;
use std::time::Instant;

struct Node {
    n: u32,
    state: u8, // 1 idle, 2 running
    foc_done_at: Option<Instant>,
    current: f32,
    last_cmd: Option<Instant>,
    velocity: f32,
    vel_interval_ms: u64,
    last_vel: Instant,
    last_status: Instant,
    pending_node_id: u8,
    invert: bool,
}

pub struct Sim {
    nodes: Vec<Node>,
    t0: Instant,
    last_step: Instant,
}

fn frame(id: u32, data: Vec<u8>) -> CanFrame {
    CanFrame { id, ext: false, rtr: false, fd: false, brs: false, esi: false, data }
}

fn with_f32(mut d: Vec<u8>, v: f32) -> Vec<u8> {
    d.extend_from_slice(&v.to_be_bytes());
    d
}

impl Sim {
    pub fn new() -> Sim {
        let now = Instant::now();
        let nodes = (1..=2)
            .map(|n| Node {
                n,
                state: 1,
                foc_done_at: None,
                current: 0.0,
                last_cmd: None,
                velocity: 0.0,
                vel_interval_ms: 10,
                last_vel: now,
                last_status: now,
                pending_node_id: n as u8,
                invert: n == 2,
            })
            .collect();
        Sim { nodes, t0: now, last_step: now }
    }

    /// A frame the host sent; returns the nodes' replies.
    pub fn on_frame(&mut self, f: &CanFrame) -> Vec<CanFrame> {
        let now = Instant::now();
        let mut out = Vec::new();
        if f.ext || f.rtr {
            return out;
        }
        // shared current frame: odd node bytes 0-3, even node bytes 4-7
        if (0x010..0x018).contains(&f.id) && f.data.len() >= 8 {
            for nd in &mut self.nodes {
                if 0x010 + (nd.n - 1) / 2 == f.id {
                    let o = (((nd.n - 1) % 2) * 4) as usize;
                    let v = f32::from_be_bytes([f.data[o], f.data[o + 1], f.data[o + 2], f.data[o + 3]]);
                    if v.is_finite() {
                        nd.current = v.clamp(-2.0, 2.0);
                        nd.last_cmd = Some(now);
                    }
                }
            }
            return out;
        }
        let Some(nd) = self.nodes.iter_mut().find(|nd| 0x020 + nd.n == f.id) else { return out };
        let tel = 0x100 + nd.n;
        let Some(&cmd) = f.data.first() else { return out };
        let reply = match cmd {
            0x10 => {
                nd.foc_done_at = Some(now + std::time::Duration::from_millis(1200));
                None
            }
            0x20 => Some(vec![0x20, 4, 1, 3, 0, 96, 0, 0]),
            0x21 => Some(with_f32(vec![0x21, *f.data.get(1).unwrap_or(&0), nd.pending_node_id, nd.invert as u8 | 0x02], 1.234)),
            0x22 => {
                let param = *f.data.get(1).unwrap_or(&0);
                let v = f.data.get(2..6).map(|b| f32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(f32::NAN);
                let ok = match param {
                    1 if (1.0..=15.0).contains(&v) => {
                        nd.pending_node_id = v as u8;
                        true
                    }
                    2 => {
                        nd.invert = v != 0.0;
                        true
                    }
                    0x10 if (1.0..=100.0).contains(&v) => {
                        nd.vel_interval_ms = v as u64;
                        true
                    }
                    _ => false,
                };
                Some(with_f32(vec![0x22, param, if ok { 0 } else { 2 }], v))
            }
            0x23 => Some(vec![0x23, *f.data.get(1).unwrap_or(&0), if nd.state == 2 { 0 } else { 3 }]),
            0x24..=0x26 => Some(vec![cmd, 0]),
            0x27 => match f.data.get(1).copied().unwrap_or(0) {
                0 => Some(vec![0x27, 0, 0, 0, 0, 3, 2, 0]),
                p @ 1..=5 => {
                    let name = b"BLDC Driver v0.2 (simulated)";
                    let mut d = vec![0x27, p];
                    for i in 0..6 {
                        d.push(*name.get((p as usize - 1) * 6 + i).unwrap_or(&0));
                    }
                    Some(d)
                }
                _ => Some(vec![0xFE, 0x27, 2]),
            },
            _ => Some(vec![0xFE, cmd, 1]),
        };
        if let Some(d) = reply {
            out.push(frame(tel, d));
        }
        out
    }

    /// Advances the motor model; returns telemetry that is due.
    pub fn tick(&mut self) -> Vec<CanFrame> {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f32().min(0.05);
        self.last_step = now;
        let t = now.duration_since(self.t0).as_secs_f32();
        let mut out = Vec::new();
        for nd in &mut self.nodes {
            let tel = 0x100 + nd.n;
            if let Some(at) = nd.foc_done_at {
                if now >= at {
                    nd.foc_done_at = None;
                    nd.state = 2;
                    out.push(frame(tel, with_f32(vec![0x10], 1.0)));
                }
            }
            let timed_out = nd.last_cmd.is_none_or(|c| now.duration_since(c).as_millis() > 100);
            let i = if nd.state == 2 && !timed_out { nd.current } else { 0.0 };
            // torque ∝ current, viscous friction, a little load ripple
            let load = 0.8 * (t * 1.7 + nd.n as f32).sin();
            let accel = 60.0 * i - 1.5 * nd.velocity - load;
            nd.velocity += accel * dt;
            if nd.state == 2 && now.duration_since(nd.last_vel).as_millis() as u64 >= nd.vel_interval_ms {
                nd.last_vel = now;
                let v = if nd.invert { -nd.velocity } else { nd.velocity };
                out.push(frame(tel, with_f32(vec![0x02], v)));
            }
            if now.duration_since(nd.last_status).as_millis() >= 100 {
                nd.last_status = now;
                let input_a = (i.abs() * 0.35 + 0.04 + 0.01 * (t * 5.0 + nd.n as f32).sin()).max(0.0);
                let mv = (24_000.0 - 400.0 * input_a + 15.0 * (t * 3.1).sin()) as u16;
                let ma = (input_a * 1000.0) as i16;
                let mut flags = 0u8;
                if nd.state == 2 && timed_out {
                    flags |= 0x01;
                }
                if nd.invert {
                    flags |= 0x04;
                }
                let mut d = vec![0x03, nd.state, flags];
                d.extend_from_slice(&mv.to_be_bytes());
                d.extend_from_slice(&ma.to_be_bytes());
                out.push(frame(tel, d));
            }
        }
        out
    }
}
