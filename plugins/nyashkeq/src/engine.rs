//! The EQ engine: 24 bands with dynamics, three processing modes, character, output section.
//!
//! - Zero Latency: every band is an analog-matched biquad cascade; left / right / stereo bands
//!   run first, mid / side bands after.
//! - Natural Phase: the same cascades, then a short FIR (from the worker thread) that corrects
//!   what the digital filters still miss (near Nyquist) to the exact analog magnitude and phase.
//! - Linear Phase: one FIR kernel (a 2 x 2 matrix for mid / side) built from the analog
//!   magnitude, crossfaded whenever the bands (or their dynamic gains) move.
//!
//! Dynamics: each dynamic band listens to a band-limited copy of the input (or the external side
//! chain), compares its level with the threshold (fixed, or following the signal's own average
//! in auto mode) with a soft knee, and moves the band's gain by up to the dynamic range. In the
//! FIR modes the detector runs on the undelayed input, so it effectively looks ahead.
use crate::conv::{BandReq, Conv, Designer, Kind, Place, Req, Worker, NB};
use crate::filters::{analog, Cascade, CascadeState, Shape, Slope, Spec};
use crate::params::{Character, PMode, Res};
use nyashk_common::dsp::{sat_asym, Bq, Coef, Os2};

#[derive(Clone, Copy, Debug)]
pub struct BandIn {
    pub used: bool,
    pub on: bool,
    pub spec: Spec,
    pub place: Place,
    pub dyn_range: f64,
    pub dyn_on: bool,
    pub thr_auto: bool,
    pub thr: f64,
    /// 0..1, 0.5 = auto
    pub att: f64,
    pub rel: f64,
    pub sc_free: bool,
    pub sc_lo: f64,
    pub sc_hi: f64,
    pub sc_ext: bool,
}

impl Default for BandIn {
    fn default() -> Self {
        Self {
            used: false,
            on: true,
            spec: Spec { shape: Shape::Bell, slope: Slope::S12, freq: 1000.0, gain_db: 0.0, q: 1.0, gq: false },
            place: Place::Stereo,
            dyn_range: 0.0,
            dyn_on: true,
            thr_auto: true,
            thr: -30.0,
            att: 0.5,
            rel: 0.5,
            sc_free: false,
            sc_lo: 20.0,
            sc_hi: 20000.0,
            sc_ext: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub bands: [BandIn; NB],
    pub pmode: PMode,
    pub res: Res,
    pub character: Character,
    pub auto_gain: bool,
    /// 1.0 = 100 %
    pub gain_scale: f64,
    pub out_db: f32,
    /// -1..1
    pub pan: f32,
    pub phase: bool,
    pub bypass: bool,
    /// band being auditioned (-1: none)
    pub solo: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { bands: [BandIn::default(); NB], pmode: PMode::Zero, res: Res::Medium, character: Character::Clean, auto_gain: false, gain_scale: 1.0, out_db: 0.0, pan: 0.0, phase: false, bypass: false, solo: -1 }
    }
}

impl BandIn {
    pub fn dynamic(&self) -> bool {
        self.spec.shape.has_gain() && self.dyn_range.abs() > 0.01
    }
}

// ------------------------------------------------------------------ detector

/// trigger filter for a band: what part of the spectrum the dynamics listen to (also used for
/// the band audition)
fn trigger_filters(fs: f32, b: &BandIn) -> (Coef, Coef) {
    let f = b.spec.freq as f32;
    let q = (b.spec.q as f32).clamp(0.4, 12.0);
    if b.sc_free {
        return (Coef::highpass(fs, (b.sc_lo as f32).min(fs * 0.45), 0.707), Coef::lowpass(fs, (b.sc_hi as f32).min(fs * 0.45), 0.707));
    }
    let f = f.clamp(10.0, fs * 0.45);
    match b.spec.shape {
        Shape::LowShelf | Shape::LowCut => (Coef::lowpass(fs, f, 0.707), Coef::THRU),
        Shape::HighShelf | Shape::HighCut | Shape::TiltShelf | Shape::FlatTilt => (Coef::highpass(fs, f, 0.707), Coef::THRU),
        _ => (Coef::bandpass(fs, f, q), Coef::THRU),
    }
}

#[derive(Clone, Copy)]
struct Det {
    c: (Coef, Coef),
    f1: [Bq; 2],
    f2: [Bq; 2],
    env: f64,
    slow: f64,
    key: [f32; 6],
}

impl Default for Det {
    fn default() -> Self {
        Self { c: (Coef::THRU, Coef::THRU), f1: Default::default(), f2: Default::default(), env: 0.0, slow: -100.0, key: [0.0; 6] }
    }
}

impl Det {
    fn configure(&mut self, fs: f32, b: &BandIn) {
        let key = [b.spec.freq as f32, b.spec.q as f32, b.spec.shape as u32 as f32, if b.sc_free { 1.0 } else { 0.0 }, b.sc_lo as f32, b.sc_hi as f32];
        if key != self.key {
            self.key = key;
            self.c = trigger_filters(fs, b);
        }
    }
    #[inline]
    fn filt(&mut self, ch: usize, x: f32) -> f32 {
        let y = self.f1[ch].tick(&self.c.0, x);
        self.f2[ch].tick(&self.c.1, y)
    }
    /// trigger power for the band's placement
    #[inline]
    fn tick(&mut self, l: f32, r: f32, nch: usize, place: Place, att: f64, rel: f64) {
        let p = if nch == 1 {
            let y = self.filt(0, l);
            y * y
        } else {
            match place {
                Place::Stereo => {
                    let a = self.filt(0, l);
                    let b = self.filt(1, r);
                    (a * a).max(b * b)
                }
                Place::Left => {
                    let a = self.filt(0, l);
                    a * a
                }
                Place::Right => {
                    let a = self.filt(1, r);
                    a * a
                }
                Place::Mid => {
                    let a = self.filt(0, 0.5 * (l + r));
                    a * a
                }
                Place::Side => {
                    let a = self.filt(0, 0.5 * (l - r));
                    a * a
                }
            }
        } as f64;
        let c = if p > self.env { att } else { rel };
        self.env = p + (self.env - p) * c;
        if self.env < 1e-20 {
            self.env = 0.0;
        }
    }
    fn level_db(&self) -> f64 {
        10.0 * (self.env + 1e-12).log10() + 3.0
    }
    fn reset(&mut self) {
        self.f1 = Default::default();
        self.f2 = Default::default();
        self.env = 0.0;
        self.slow = -100.0;
    }
}

// ------------------------------------------------------------------ band

#[derive(Clone, Copy)]
struct Band {
    active: bool,
    fade: f32,
    /// what the cascade was designed for (gain includes the dynamic part)
    des: Spec,
    place: Place,
    casc: Cascade,
    st: [CascadeState; 2],
    old: Option<(Cascade, [CascadeState; 2], Place)>,
    xf: f32,
    det: Det,
    dyn_db: f64,
    /// a full (method-searching) design is due once the settings stop moving
    settle: i32,
    stat_gain: f64,
    att_c: f64,
    rel_c: f64,
}

impl Default for Band {
    fn default() -> Self {
        Self { active: false, fade: 0.0, des: BandIn::default().spec, place: Place::Stereo, casc: Cascade::default(), st: Default::default(), old: None, xf: 1.0, det: Det::default(), dyn_db: 0.0, settle: -1, stat_gain: 0.0, att_c: 0.0, rel_c: 0.0 }
    }
}

#[inline]
fn ss(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

impl Band {
    /// one band on one sample (l, r); `nch` 1 or 2
    #[inline]
    fn run(&mut self, l: &mut f64, r: &mut f64, nch: usize) {
        let fade = ss(self.fade) as f64;
        let xf = ss(self.xf) as f64;
        let one = |casc: &Cascade, st: &mut [CascadeState; 2], place: Place, l: f64, r: f64| -> (f64, f64) {
            if nch == 1 {
                return match place {
                    Place::Right | Place::Side => (l, r),
                    _ => (st[0].tick(casc, l), r),
                };
            }
            match place {
                Place::Stereo => (st[0].tick(casc, l), st[1].tick(casc, r)),
                Place::Left => (st[0].tick(casc, l), r),
                Place::Right => (l, st[1].tick(casc, r)),
                Place::Mid => {
                    let (m, s) = (0.5 * (l + r), 0.5 * (l - r));
                    let m = st[0].tick(casc, m);
                    (m + s, m - s)
                }
                Place::Side => {
                    let (m, s) = (0.5 * (l + r), 0.5 * (l - r));
                    let s = st[0].tick(casc, s);
                    (m + s, m - s)
                }
            }
        };
        let (mut yl, mut yr) = one(&self.casc, &mut self.st, self.place, *l, *r);
        if let Some((oc, ost, op)) = self.old.as_mut() {
            let (ol, or) = one(oc, ost, *op, *l, *r);
            yl = ol + (yl - ol) * xf;
            yr = or + (yr - or) * xf;
        }
        *l += (yl - *l) * fade;
        *r += (yr - *r) * fade;
    }
}

// ------------------------------------------------------------------ engine

pub struct Engine {
    pub fs: f64,
    sc: usize,
    nch: usize,
    bands: [Band; NB],
    lr_order: [u8; NB],
    n_lr: usize,
    ms_order: [u8; NB],
    n_ms: usize,
    tick: u32,
    cur: Settings,
    /// the mode the audio is running in now
    run_mode: PMode,
    run_res: Res,
    /// 0..1 output fade for mode switches
    mode_x: f32,
    switching: bool,
    wait: u32,
    nat: Conv,
    lin: Conv,
    nat_n: usize,
    worker: Worker,
    /// offline rendering: kernels are designed right here, in step with the audio
    sync: Option<Designer>,
    kern_dirty: bool,
    since_post: usize,
    latency: usize,
    // dry delay (bypass / audition)
    dly: [Vec<f32>; 2],
    dpos: usize,
    byp_x: f32,
    solo_x: f32,
    solo_band: i32,
    solo_f: [Bq; 4],
    solo_c: (Coef, Coef),
    // output
    os: [Os2; 2],
    char_x: f32,
    char_run: Character,
    ag_db: f64,
    ag_target: f64,
    ag_dirty: bool,
    ag_coef: f64,
    pub dyn_out: [f32; NB],
    pub lvl_out: [f32; NB],
    pub thr_out: [f32; NB],
    /// dynamics are off because of the linear-phase resolution
    pub dyn_blocked: bool,
}

impl Engine {
    pub fn new(fs: f64, nch: usize) -> Self {
        let sc = ((fs / 48000.0).round().max(1.0) as usize).next_power_of_two().min(4);
        let nat_n = 512 * sc;
        let max_lin = Res::Max.n48() * sc;
        let max_lat = max_lin / 2 + 512 * sc;
        let dl = (max_lat + 64).next_power_of_two();
        Self {
            fs,
            sc,
            nch: nch.clamp(1, 2),
            bands: [Band::default(); NB],
            lr_order: [0; NB],
            n_lr: 0,
            ms_order: [0; NB],
            n_ms: 0,
            tick: 0,
            cur: Settings::default(),
            run_mode: PMode::Zero,
            run_res: Res::Medium,
            mode_x: 1.0,
            switching: false,
            wait: 0,
            nat: Conv::new(128 * sc, nat_n),
            lin: Conv::new(512 * sc, max_lin),
            nat_n,
            worker: Worker::new(),
            sync: None,
            kern_dirty: true,
            since_post: usize::MAX / 2,
            latency: 0,
            dly: [vec![0.0; dl], vec![0.0; dl]],
            dpos: 0,
            byp_x: 0.0,
            solo_x: 0.0,
            solo_band: -1,
            solo_f: Default::default(),
            solo_c: (Coef::THRU, Coef::THRU),
            os: [Os2::new(fs as f32), Os2::new(fs as f32)],
            char_x: 0.0,
            char_run: Character::Clean,
            ag_db: 0.0,
            ag_target: 0.0,
            ag_dirty: true,
            ag_coef: (-1.0 / (0.15 * fs)).exp(),
            dyn_out: [0.0; NB],
            lvl_out: [-120.0; NB],
            thr_out: [-30.0; NB],
            dyn_blocked: false,
        }
    }

    /// offline (bounce) / test rendering: design kernels synchronously so the result does not
    /// depend on how fast the computer is
    pub fn set_offline(&mut self, on: bool) {
        if on && self.sync.is_none() {
            self.sync = Some(Designer::default());
        } else if !on {
            self.sync = None;
        }
    }

    pub fn latency(&self) -> usize {
        self.latency
    }

    fn latency_for(&self, m: PMode, r: Res) -> usize {
        match m {
            PMode::Zero => 0,
            PMode::Natural => self.nat_n / 2 + self.nat.p(),
            PMode::Linear => r.n48() * self.sc / 2 + self.lin.p(),
        }
    }

    pub fn reset(&mut self) {
        for b in self.bands.iter_mut() {
            b.st = Default::default();
            b.old = None;
            b.det.reset();
        }
        self.nat.reset();
        self.lin.reset();
        for d in self.dly.iter_mut() {
            d.iter_mut().for_each(|v| *v = 0.0);
        }
        for o in self.os.iter_mut() {
            o.reset();
        }
        self.solo_f = Default::default();
    }

    /// set the latency straight away (initialize): switches modes without a fade
    pub fn prime(&mut self, s: &Settings) {
        self.cur = *s;
        self.run_mode = s.pmode;
        self.run_res = s.res;
        self.latency = self.latency_for(s.pmode, s.res);
        self.kern_dirty = true;
        // the FIR modes fade in once their first kernel is there
        self.mode_x = if s.pmode == PMode::Zero { 1.0 } else { 0.0 };
        self.wait = 0;
    }

    /// new settings (call at least every block; cheap when nothing changed)
    pub fn set(&mut self, s: &Settings) {
        self.cur = *s;
        if (s.pmode != self.run_mode || (s.pmode == PMode::Linear && s.res != self.run_res)) && !self.switching {
            self.switching = true;
        }
    }

    fn scaled(&self, b: &BandIn) -> (Spec, f64) {
        let k = self.cur.gain_scale;
        let mut sp = b.spec;
        sp.gain_db = (sp.gain_db * k).clamp(-30.0, 30.0);
        sp.slope = sp.slope.for_shape(sp.shape);
        (sp, b.dyn_range * k)
    }

    fn conv_kind(&self) -> Option<Kind> {
        match self.run_mode {
            PMode::Zero => None,
            PMode::Natural => Some(Kind::Natural(self.nat_n)),
            PMode::Linear => Some(Kind::Linear(self.run_res.n48() * self.sc)),
        }
    }

    /// the control-rate part (every 16 samples)
    fn control(&mut self) {
        let fs = self.fs;
        let dyn_ok = self.run_mode != PMode::Linear || self.run_res.dynamics_ok();
        self.dyn_blocked = !dyn_ok && self.cur.bands.iter().any(|b| b.used && b.dynamic());
        let fstep = 16.0 / (0.010 * fs as f32);
        let xstep = 16.0 / (0.020 * fs as f32);
        self.n_lr = 0;
        self.n_ms = 0;
        for i in 0..NB {
            let bi = self.cur.bands[i];
            let (sp, range) = self.scaled(&bi);
            let want = bi.used && bi.on;
            let b = &mut self.bands[i];
            // dynamics
            let dynamic = bi.dynamic() && bi.dyn_on && want && dyn_ok;
            if bi.used && bi.dynamic() {
                b.det.configure(fs as f32, &bi);
                let lvl = b.det.level_db();
                if lvl > -80.0 {
                    let a = 1.0 - (-16.0 / (1.5 * fs)).exp();
                    b.det.slow += (lvl - b.det.slow) * a;
                }
                let thr = if bi.thr_auto { b.det.slow + 2.0 } else { bi.thr };
                self.lvl_out[i] = lvl as f32;
                self.thr_out[i] = thr as f32;
                let target = if dynamic && lvl > -90.0 {
                    let over = lvl - thr;
                    let o = if over <= -3.0 {
                        0.0
                    } else if over < 3.0 {
                        (over + 3.0).powi(2) / 12.0
                    } else {
                        over
                    };
                    if range < 0.0 {
                        -o.min(-range)
                    } else {
                        o.min(range)
                    }
                } else {
                    0.0
                };
                b.dyn_db = target;
            } else {
                b.dyn_db = 0.0;
                self.lvl_out[i] = -120.0;
            }
            self.dyn_out[i] = b.dyn_db as f32;
            let mut eff = sp;
            if eff.shape.has_gain() {
                eff.gain_db = (sp.gain_db + b.dyn_db).clamp(-30.0, 30.0);
            }
            // on / off
            if want && !b.active {
                b.active = true;
                b.fade = 0.0;
                b.des = eff;
                b.place = bi.place;
                b.casc = Cascade::from_analog(&analog(&eff), fs);
                b.st = Default::default();
                b.old = None;
                b.xf = 1.0;
                b.settle = -1;
                self.kern_dirty = true;
                self.ag_dirty = true;
            }
            if !b.active {
                continue;
            }
            b.fade = if want { (b.fade + fstep).min(1.0) } else { (b.fade - fstep).max(0.0) };
            if !want && b.fade <= 0.0 {
                b.active = false;
                b.old = None;
                self.kern_dirty = true;
                self.ag_dirty = true;
                continue;
            }
            if b.old.is_some() {
                b.xf = (b.xf + xstep).min(1.0);
                if b.xf >= 1.0 {
                    b.old = None;
                }
            }
            let structural = eff.shape != b.des.shape || eff.slope != b.des.slope || bi.place != b.place;
            let moved = (eff.freq / b.des.freq).ln().abs() > 1e-5 || (eff.q / b.des.q).ln().abs() > 1e-5 || (eff.gain_db - b.des.gain_db).abs() > 0.01 || eff.gq != b.des.gq;
            if structural {
                // crossfade from the old filter (its states keep running) to the new one
                b.old = Some((b.casc, b.st, b.place));
                b.xf = 0.0;
                b.casc = Cascade::from_analog(&analog(&eff), fs);
                b.st = Default::default();
                b.des = eff;
                b.place = bi.place;
                b.settle = -1;
                self.kern_dirty = true;
                self.ag_dirty = true;
            } else if moved {
                let static_move = (eff.freq / b.des.freq).ln().abs() > 1e-5 || (eff.q / b.des.q).ln().abs() > 1e-5 || eff.gq != b.des.gq || (sp.gain_db - b.stat_gain).abs() > 0.01;
                b.casc = Cascade::quick(&analog(&eff), fs, &b.casc);
                b.des = eff;
                b.settle = 48;
                self.kern_dirty = true;
                if static_move {
                    self.ag_dirty = true;
                }
            } else if b.settle > 0 {
                b.settle -= 1;
            } else if b.settle == 0 {
                // stopped moving: search the best digital design for every section again
                b.settle = -1;
                b.casc = Cascade::from_analog(&analog(&b.des), fs);
            }
            if matches!(b.place, Place::Mid | Place::Side) {
                self.ms_order[self.n_ms] = i as u8;
                self.n_ms += 1;
            } else {
                self.lr_order[self.n_lr] = i as u8;
                self.n_lr += 1;
            }
            b.stat_gain = sp.gain_db;
        }
        // detector time constants (program dependent: from the band's frequency)
        for i in 0..NB {
            if self.bands[i].active && self.cur.bands[i].dynamic() {
                let (a, r) = self.det_coefs(i);
                self.bands[i].att_c = a;
                self.bands[i].rel_c = r;
            }
        }
        // auto gain from the static curve
        if self.ag_dirty && self.tick % 128 == 0 {
            self.ag_dirty = false;
            let mut tot = 0.0;
            let npts = 40;
            for i in 0..NB {
                let bi = &self.cur.bands[i];
                if !(bi.used && bi.on) {
                    continue;
                }
                let (sp, _) = self.scaled(bi);
                if sp.is_flat() {
                    continue;
                }
                let w = match bi.place {
                    Place::Stereo | Place::Mid => 1.0,
                    Place::Left | Place::Right => 0.5,
                    Place::Side => 0.25,
                };
                let a = analog(&sp);
                for k in 0..npts {
                    let f = 30.0 * (15000.0f64 / 30.0).powf(k as f64 / (npts - 1) as f64);
                    tot += w * a.db(f).max(-40.0);
                }
            }
            self.ag_target = (-tot / npts as f64).clamp(-24.0, 24.0);
        }
        // kernels
        if let Some(kind) = self.conv_kind() {
            let p = if self.run_mode == PMode::Linear { self.lin.p() } else { self.nat.p() };
            if self.kern_dirty && self.since_post >= p {
                let mut r = Req { kind, fs, p, mono: self.nch == 1, bands: [BandReq { on: false, spec: BandIn::default().spec, place: Place::Stereo }; NB] };
                for i in 0..NB {
                    let b = &self.bands[i];
                    r.bands[i] = BandReq { on: b.active && self.cur.bands[i].used && self.cur.bands[i].on, spec: b.des, place: b.place };
                }
                if let Some(d) = self.sync.as_mut() {
                    let k = d.build(&r);
                    self.kern_dirty = false;
                    self.since_post = 0;
                    match self.run_mode {
                        PMode::Linear => self.lin.set_kernel(k),
                        _ => self.nat.set_kernel(k),
                    }
                } else if self.worker.post(&r) {
                    self.kern_dirty = false;
                    self.since_post = 0;
                }
            }
            if let Some(k) = self.worker.take() {
                if k.kind == kind {
                    match self.run_mode {
                        PMode::Linear => self.lin.set_kernel(k),
                        _ => self.nat.set_kernel(k),
                    }
                } else if let Some(k) = self.worker.recycle(k) {
                    drop(k);
                }
            }
            for c in [&mut self.lin, &mut self.nat] {
                if let Some(k) = c.retired.take() {
                    c.retired = self.worker.recycle(k);
                }
            }
        }
        // audition filter
        if self.cur.solo != self.solo_band && self.solo_x <= 0.0 {
            self.solo_band = self.cur.solo;
            self.solo_f = Default::default();
        }
        if self.solo_band >= 0 {
            let mut bi = self.cur.bands[self.solo_band as usize];
            bi.sc_free = false;
            self.solo_c = trigger_filters(fs as f32, &bi);
        }
    }

    /// detector time constants for band i (per-sample coefficients)
    fn det_coefs(&self, i: usize) -> (f64, f64) {
        let b = &self.cur.bands[i];
        let f = if b.sc_free { (b.sc_lo * b.sc_hi).sqrt() } else { b.spec.freq };
        let a0 = (2000.0 / f.max(10.0)).clamp(0.5, 30.0);
        let r0 = (a0 * 10.0).clamp(30.0, 300.0);
        let fa = 10f64.powf((b.att - 0.5) * 2.0);
        let fr = 10f64.powf((b.rel - 0.5) * 2.0);
        let c = |ms: f64| (-1.0 / (ms * 0.001 * self.fs)).exp();
        (c(a0 * fa), c(r0 * fr))
    }

    /// One sample. `x`: input, `sc`: external side chain. Returns the output.
    pub fn process(&mut self, x: [f32; 2], sc: [f32; 2]) -> [f32; 2] {
        let nch = self.nch;
        let mut x = x;
        for v in x.iter_mut() {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        // mode switch: fade out, switch, wait for the first kernel, fade in
        let mstep = 1.0 / (0.008 * self.fs as f32);
        if self.switching {
            self.mode_x = (self.mode_x - mstep).max(0.0);
            if self.mode_x <= 0.0 {
                self.switching = false;
                self.run_mode = self.cur.pmode;
                self.run_res = self.cur.res;
                self.latency = self.latency_for(self.run_mode, self.run_res);
                self.nat.reset();
                self.lin.reset();
                if let Some(k) = self.nat.cur.take() {
                    self.nat.retired = self.nat.retired.take().or(Some(k));
                }
                if let Some(k) = self.lin.cur.take() {
                    if self.lin.retired.is_none() {
                        self.lin.retired = Some(k);
                    }
                }
                for b in self.bands.iter_mut() {
                    b.st = Default::default();
                    b.old = None;
                }
                self.kern_dirty = true;
                self.since_post = usize::MAX / 2;
                self.wait = 0;
            }
        } else {
            let ready = match self.run_mode {
                PMode::Zero => true,
                PMode::Natural => self.nat.has_kernel(),
                PMode::Linear => self.lin.has_kernel(),
            };
            if ready {
                self.wait = self.wait.saturating_add(1);
            }
            // fade in once the new path's delay line holds signal (no abrupt onset)
            if (ready && self.wait as usize > self.latency + 16) || self.mode_x > 0.0 {
                self.mode_x = (self.mode_x + mstep).min(1.0);
            }
        }
        if self.tick % 16 == 0 {
            self.control();
        }
        self.tick = self.tick.wrapping_add(1);
        self.since_post = self.since_post.saturating_add(1);

        // detectors
        for i in 0..NB {
            if !self.bands[i].active || !self.cur.bands[i].dynamic() {
                continue;
            }
            let (tl, tr) = if self.cur.bands[i].sc_ext { (sc[0], sc[1]) } else { (x[0], x[1]) };
            let b = &mut self.bands[i];
            let (place, a, r) = (b.place, b.att_c, b.rel_c);
            b.det.tick(tl, tr, nch, place, a, r);
        }

        // dry delay (for bypass / audition, aligned with the latency)
        let mask = self.dly[0].len() - 1;
        self.dly[0][self.dpos & mask] = x[0];
        self.dly[1][self.dpos & mask] = x[1];
        let rd = self.dpos.wrapping_sub(self.latency) & mask;
        let dry = [self.dly[0][rd], self.dly[1][rd]];
        self.dpos = self.dpos.wrapping_add(1);

        // EQ
        let (mut l, mut r) = (x[0] as f64, if nch == 2 { x[1] as f64 } else { 0.0 });
        let mut y = match self.run_mode {
            PMode::Zero | PMode::Natural => {
                for k in 0..self.n_lr {
                    let i = self.lr_order[k] as usize;
                    self.bands[i].run(&mut l, &mut r, nch);
                }
                for k in 0..self.n_ms {
                    let i = self.ms_order[k] as usize;
                    self.bands[i].run(&mut l, &mut r, nch);
                }
                let y = [l as f32, r as f32];
                if self.run_mode == PMode::Natural {
                    self.nat.tick(y)
                } else {
                    y
                }
            }
            PMode::Linear => self.lin.tick([l as f32, r as f32]),
        };
        if nch == 1 {
            y[1] = 0.0;
        }
        let mx = ss(self.mode_x);
        y[0] *= mx;
        y[1] *= mx;

        // character
        let cstep = 1.0 / (0.015 * self.fs as f32);
        if self.cur.character != self.char_run {
            self.char_x = (self.char_x - cstep).max(0.0);
            if self.char_x <= 0.0 {
                self.char_run = self.cur.character;
                for o in self.os.iter_mut() {
                    o.reset();
                }
            }
        } else if self.char_run != Character::Clean {
            self.char_x = (self.char_x + cstep).min(1.0);
        }
        if self.char_run != Character::Clean || self.char_x > 0.0 {
            let w = ss(self.char_x);
            let (drive, bias) = match self.char_run {
                Character::Subtle => (0.55, 0.02),
                Character::Warm => (1.1, 0.09),
                Character::Clean => (0.5, 0.0),
            };
            for c in 0..nch {
                let a = y[c];
                let s = self.os[c].process(a, |v| sat_asym(v * drive, bias) / drive);
                y[c] = a + (s - a) * w;
            }
        }

        // output: auto gain, gain, pan, phase
        let ag = if self.cur.auto_gain { self.ag_target } else { 0.0 };
        self.ag_db = ag + (self.ag_db - ag) * self.ag_coef;
        let g = 10f32.powf((self.cur.out_db + self.ag_db as f32) / 20.0) * if self.cur.phase { -1.0 } else { 1.0 };
        if nch == 2 {
            let p = self.cur.pan;
            y[0] *= g * (1.0 - p).min(1.0);
            y[1] *= g * (1.0 + p).min(1.0);
        } else {
            y[0] *= g;
        }

        // audition
        let sstep = 1.0 / (0.010 * self.fs as f32);
        let solo_on = self.cur.solo >= 0 && self.cur.solo == self.solo_band;
        self.solo_x = if solo_on { (self.solo_x + sstep).min(1.0) } else { (self.solo_x - sstep).max(0.0) };
        if self.solo_x > 0.0 {
            let w = ss(self.solo_x);
            for c in 0..nch {
                let a = self.solo_f[c].tick(&self.solo_c.0, dry[c]);
                let s = self.solo_f[2 + c].tick(&self.solo_c.1, a);
                y[c] += (s - y[c]) * w;
            }
        }

        // bypass
        let bstep = 1.0 / (0.010 * self.fs as f32);
        self.byp_x = if self.cur.bypass { (self.byp_x + bstep).min(1.0) } else { (self.byp_x - bstep).max(0.0) };
        if self.byp_x > 0.0 {
            let w = ss(self.byp_x);
            for c in 0..2 {
                y[c] += (dry[c] - y[c]) * w;
            }
        }
        for v in y.iter_mut() {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        // once per block-ish: kill denormals in the filter states
        if self.tick % 1024 == 0 {
            for b in self.bands.iter_mut() {
                for s in b.st.iter_mut() {
                    s.sanitize();
                }
            }
        }
        y
    }
}
