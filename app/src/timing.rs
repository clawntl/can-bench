//! Bit timing search, following HUD ECU Hacker's own calculator (ECU_Hacker.exe, decompiled IL):
//!   driver     0x060009f1  samplepoint checks, prescaler loop 1..=min(max brp), keep the better candidate
//!   per brp    0x06000a12  tq = clock / brp / baud; seg1+1 = tq * sp‰ / 1000; seg2 = tq - (seg1+1); baud error ≤ 0.5 %
//!   helpers    0x06000a08  minimum tq = 3;  0x06000a09/0a/0b tq = 1 + seg1 + seg2;
//!              0x06000a0c  sp‰ = 1000 * (1 + seg1) / tq;  0x06000a0d baud = clock / brp / tq
//!              0x06000a0e/0f  error % = |actual - wanted| / wanted * 100
//!              0x06000a10  perfect = both errors < 0.01 %;  0x06000a11 compare: baud error first, then samplepoint error
//! baud = clock / brp / (1 + seg1 + seg2); samplepoint = (1 + seg1) / (1 + seg1 + seg2).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub brp_min: u32,
    pub brp_max: u32,
    pub seg1_max: u32,
    pub seg2_max: u32,
    pub sjw_max: u32,
}

impl Limits {
    pub const fn nominal_g4() -> Self {
        Limits { brp_min: 1, brp_max: 512, seg1_max: 256, seg2_max: 128, sjw_max: 128 }
    }
    pub const fn data_g4() -> Self {
        Limits { brp_min: 1, brp_max: 32, seg1_max: 32, seg2_max: 16, sjw_max: 16 }
    }
}

/// Requested samplepoint limits checked by 0x060009f1 (in tenths of a percent).
pub const SP_MIN_PM: u32 = 500;
pub const SP_MAX_NOMINAL_PM: u32 = 900;
pub const SP_MAX_DATA_PM: u32 = 800;
/// 0x06000a08
pub const MIN_TQ: u32 = 3;
/// 0x06000a12: a candidate whose baudrate is off by more than this is rejected (percent).
pub const MAX_BAUD_ERROR: f64 = 0.5;
/// 0x06000a10: below this both errors count as a perfect match (percent).
pub const PERFECT: f64 = 0.01;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub brp: u32,
    pub seg1: u32,
    pub seg2: u32,
    pub sjw: u32,
}

impl Timing {
    pub fn tq(&self) -> u32 {
        1 + self.seg1 + self.seg2
    }
    pub fn baud(&self, clock: u32) -> f64 {
        clock as f64 / self.brp as f64 / self.tq() as f64
    }
    pub fn samplepoint(&self) -> f64 {
        100.0 * (1 + self.seg1) as f64 / self.tq() as f64
    }
}

pub fn fmt_baud(b: f64) -> String {
    let r = b.round() as u64;
    if r >= 1_000_000 && r % 1_000_000 == 0 {
        format!("{}M", r / 1_000_000)
    } else if r >= 1_000_000 {
        format!("{:.3}M", b / 1e6)
    } else if r >= 1000 && r % 1000 == 0 {
        format!("{}k", r / 1000)
    } else if r >= 1000 {
        format!("{:.3}k", b / 1e3)
    } else {
        format!("{r}")
    }
}

/// One candidate with its errors in percent.
#[derive(Clone, Copy, Debug)]
struct Cand {
    t: Timing,
    baud_err: f64,
    sp_err: f64,
}

impl Cand {
    fn perfect(&self) -> bool {
        self.baud_err < PERFECT && self.sp_err < PERFECT
    }
    /// 0x06000a11: 1 = better than (b, s), -1 = worse, 0 = equal
    fn cmp(&self, b: f64, s: f64) -> i32 {
        if self.baud_err < b {
            1
        } else if self.baud_err > b {
            -1
        } else if self.sp_err < s {
            1
        } else if self.sp_err > s {
            -1
        } else {
            0
        }
    }
}

/// 0x06000a12: the timing for one prescaler, or None if it can't work.
fn for_brp(clock: u32, baud: u32, sp_pm: u32, brp: u32, lim: &Limits) -> Option<Cand> {
    if brp == 0 || baud == 0 {
        return None;
    }
    let tq = clock / brp / baud;
    if tq < MIN_TQ || tq > 1 + lim.seg1_max + lim.seg2_max {
        return None;
    }
    let s1p1 = (tq as u64 * sp_pm as u64 / 1000) as u32;
    let mut seg2 = tq.saturating_sub(s1p1);
    let mut seg1 = s1p1.saturating_sub(1);
    if seg2 > lim.seg2_max {
        seg2 = lim.seg2_max;
        seg1 = tq - 1 - seg2;
    }
    if seg1 < 1 || seg2 < 1 || seg1 > lim.seg1_max {
        return None;
    }
    let t = Timing { brp, seg1, seg2, sjw: seg1.min(seg2).min(lim.sjw_max).max(1) };
    let actual_baud = clock / brp / tq; // 0x06000a0d, whole numbers
    let baud_err = (actual_baud as f64 - baud as f64).abs() / baud as f64 * 100.0;
    if baud_err > MAX_BAUD_ERROR {
        return None;
    }
    let actual_pm = 1000 * (1 + seg1) / tq; // 0x06000a0c
    let sp_err = (actual_pm as f64 - sp_pm as f64).abs() / sp_pm as f64 * 100.0;
    Some(Cand { t, baud_err, sp_err })
}

fn to_pm(sp: f64) -> u32 {
    (sp * 10.0).round() as u32
}

fn check_sp(sp_pm: u32, max: u32, what: &str) -> Result<(), String> {
    if sp_pm < SP_MIN_PM || sp_pm > max {
        return Err(format!(
            "The {what} samplepoint must be between {:.1} % and {:.1} %.",
            SP_MIN_PM as f64 / 10.0,
            max as f64 / 10.0
        ));
    }
    Ok(())
}

fn no_match(clock: u32, baud: u32) -> String {
    format!("{} baud cannot be made from a {} MHz CAN clock", fmt_baud(baud as f64), clock / 1_000_000)
}

/// Classic CAN: returns the timing and whether it is a perfect match.
pub fn calc(clock: u32, baud: u32, sp: f64, lim: &Limits) -> Result<(Timing, bool), String> {
    let sp_pm = to_pm(sp);
    check_sp(sp_pm, SP_MAX_NOMINAL_PM, "nominal")?;
    let mut best: Option<Cand> = None;
    for brp in lim.brp_min.max(1)..=lim.brp_max {
        if let Some(c) = for_brp(clock, baud, sp_pm, brp, lim) {
            if best.map_or(true, |b| c.cmp(b.baud_err, b.sp_err) > 0) {
                best = Some(c);
                if c.perfect() {
                    break;
                }
            }
        }
    }
    best.map(|c| (c.t, c.perfect())).ok_or_else(|| no_match(clock, baud))
}

/// CAN FD: one shared prescaler for nominal and data (0x060009f1 loops 1..=min of both limits).
/// Returns (nominal, data, perfect).
pub fn calc_fd(clock: u32, nom: (u32, f64), data: (u32, f64), ln: &Limits, ld: &Limits) -> Result<(Timing, Timing, bool), String> {
    let (np, dp) = (to_pm(nom.1), to_pm(data.1));
    check_sp(np, SP_MAX_NOMINAL_PM, "nominal")?;
    check_sp(dp, SP_MAX_DATA_PM, "data")?;
    let mut best: Option<(Cand, Cand)> = None;
    for brp in ln.brp_min.max(ld.brp_min).max(1)..=ln.brp_max.min(ld.brp_max) {
        let (Some(n), Some(d)) = (for_brp(clock, nom.0, np, brp, ln), for_brp(clock, data.0, dp, brp, ld)) else { continue };
        let better = match &best {
            None => true,
            Some((bn, bd)) => {
                let (cn, cd) = (n.cmp(bn.baud_err, bn.sp_err), d.cmp(bd.baud_err, bd.sp_err));
                cn >= 0 && cd >= 0 && (cn > 0 || cd > 0)
            }
        };
        if better {
            best = Some((n, d));
            if n.perfect() && d.perfect() {
                break;
            }
        }
    }
    match best {
        Some((n, d)) => Ok((n.t, d.t, n.perfect() && d.perfect())),
        None => Err(format!("{} / {} cannot share one prescaler on a {} MHz CAN clock", fmt_baud(nom.0 as f64), fmt_baud(data.0 as f64), clock / 1_000_000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_example() {
        // Manual: 160 MHz, 500k 87.5 % / 2M 75 % → Prescaler 2, Seg1 139, Seg2 20 / Prescaler 2, Seg1 29, Seg2 10
        let (n, d, ok) = calc_fd(160_000_000, (500_000, 87.5), (2_000_000, 75.0), &Limits::nominal_g4(), &Limits::data_g4()).unwrap();
        assert!(ok);
        assert_eq!((n.brp, n.seg1, n.seg2), (2, 139, 20));
        assert_eq!((d.brp, d.seg1, d.seg2), (2, 29, 10));
    }
    #[test]
    fn manual_classic() {
        // 160 MHz, 500k 75 %: the manual's own example result baud/sp must be exact
        let (t, ok) = calc(160_000_000, 500_000, 75.0, &Limits::nominal_g4()).unwrap();
        assert!(ok);
        assert_eq!(t.baud(160_000_000).round() as u32, 500_000);
        assert!((t.samplepoint() - 75.0).abs() < 0.01);
    }
    #[test]
    fn limits() {
        assert!(calc(160_000_000, 500_000, 95.0, &Limits::nominal_g4()).is_err());
        assert!(calc_fd(160_000_000, (500_000, 80.0), (2_000_000, 85.0), &Limits::nominal_g4(), &Limits::data_g4()).is_err());
    }
}
