//! FIR side of the EQ: uniformly partitioned FFT convolution (2 x 2 matrix of kernels, so mid /
//! side and left / right bands mix correctly), kernel design for Linear Phase and for the Natural
//! Phase correction, and the worker thread that designs kernels off the audio thread.
use crate::filters::{analog, Analog, Cascade, Spec};
use nih_plug::prelude::Enum;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

type C32 = Complex<f32>;

pub const NB: usize = 24;

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Place {
    #[name = "Stereo"]
    Stereo,
    #[name = "Left"]
    Left,
    #[name = "Right"]
    Right,
    #[name = "Mid"]
    Mid,
    #[name = "Side"]
    Side,
}

impl Place {
    pub fn short(self) -> &'static str {
        match self {
            Place::Stereo => "ST",
            Place::Left => "L",
            Place::Right => "R",
            Place::Mid => "M",
            Place::Side => "S",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// linear phase, kernel length n
    Linear(usize),
    /// natural phase correction, kernel length n
    Natural(usize),
}

impl Kind {
    pub fn n(self) -> usize {
        match self {
            Kind::Linear(n) | Kind::Natural(n) => n,
        }
    }
}

/// One band as the kernel designer sees it.
#[derive(Clone, Copy, Debug)]
pub struct BandReq {
    pub on: bool,
    pub spec: Spec,
    pub place: Place,
}

/// A kernel design request (plain data: copied, never allocated on the audio thread).
#[derive(Clone, Copy)]
pub struct Req {
    pub kind: Kind,
    pub fs: f64,
    pub p: usize,
    pub mono: bool,
    pub bands: [BandReq; NB],
}

/// Kernel spectra, partitioned. Entry e = out * 2 + in (LL, LR, RL, RR).
pub struct Kernel {
    pub kind: Kind,
    pub p: usize,
    pub nparts: usize,
    pub h: [Vec<C32>; 4],
    pub used: [bool; 4],
}

// ------------------------------------------------------------------ design

/// complex helpers in f64
#[derive(Clone, Copy, Debug)]
struct Cx(f64, f64);
impl Cx {
    fn mul(self, o: Cx) -> Cx {
        Cx(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }
    fn add(self, o: Cx) -> Cx {
        Cx(self.0 + o.0, self.1 + o.1)
    }
    fn sub(self, o: Cx) -> Cx {
        Cx(self.0 - o.0, self.1 - o.1)
    }
    fn half(self) -> Cx {
        Cx(self.0 * 0.5, self.1 * 0.5)
    }
}

/// 2 x 2 complex matrix [[a, b], [c, d]]
#[derive(Clone, Copy)]
struct M2([Cx; 4]);
impl M2 {
    fn diag(a: Cx, d: Cx) -> M2 {
        M2([a, Cx(0.0, 0.0), Cx(0.0, 0.0), d])
    }
}

/// a / d, regularised: 1 where both are (near) zero
fn ratio(a: Cx, d: Cx) -> Cx {
    let e = 1e-9;
    let m = d.0 * d.0 + d.1 * d.1 + e;
    let num = a.mul(Cx(d.0, -d.1)).add(Cx(e, 0.0));
    Cx(num.0 / m, num.1 / m)
}

/// matrix written in the mid / side basis -> left / right basis (L = M + S, R = M - S)
fn from_ms(x: [Cx; 4]) -> M2 {
    // P X P^-1, P = [[1, 1], [1, -1]], P^-1 = P / 2
    let [a, b, c, d] = x;
    let ll = a.add(b).add(c).add(d).half();
    let lr = a.sub(b).add(c).sub(d).half();
    let rl = a.add(b).sub(c).sub(d).half();
    let rr = a.sub(b).sub(c).add(d).half();
    M2([ll, lr, rl, rr])
}

pub struct Designer {
    planner: RealFftPlanner<f32>,
}

impl Default for Designer {
    fn default() -> Self {
        Self { planner: RealFftPlanner::new() }
    }
}

impl Designer {
    pub fn build(&mut self, r: &Req) -> Box<Kernel> {
        let n = r.kind.n();
        let bins = n / 2 + 1;
        let fs = r.fs;
        // the bands that do something, with their analog (and, for Natural, digital) response
        let mut bands: Vec<(Analog, Option<Cascade>, crate::conv::Place)> = Vec::new();
        for b in r.bands.iter().filter(|b| b.on && !b.spec.is_flat()) {
            let a = analog(&b.spec);
            let d = match r.kind {
                Kind::Natural(_) => Some(Cascade::from_analog(&a, fs)),
                Kind::Linear(_) => None,
            };
            if r.mono && matches!(b.place, Place::Right | Place::Side) {
                continue;
            }
            bands.push((a, d, b.place));
        }
        let cross = !r.mono && bands.iter().any(|b| matches!(b.2, Place::Mid | Place::Side));
        let used = if r.mono { [true, false, false, false] } else { [true, cross, cross, true] };
        // the 2 x 2 response per bin
        let mut spec: [Vec<Cx>; 4] = Default::default();
        for s in spec.iter_mut() {
            s.resize(bins, Cx(0.0, 0.0));
        }
        let one = Cx(1.0, 0.0);
        for k in 0..bins {
            let f = (k as f64 * fs / n as f64).max(0.5);
            // per domain: (analog, digital) products
            let mut acc = [(one, one); 5];
            for (a, d, place) in &bands {
                let ha = match r.kind {
                    Kind::Linear(_) => Cx(a.mag(f), 0.0),
                    Kind::Natural(_) => {
                        let (re, im) = a.h(f);
                        Cx(re, im)
                    }
                };
                let hd = match d {
                    Some(c) => {
                        let (re, im) = c.h(f, fs);
                        Cx(re, im)
                    }
                    None => one,
                };
                let i = *place as usize;
                acc[i] = (acc[i].0.mul(ha), acc[i].1.mul(hd));
            }
            let st = acc[Place::Stereo as usize];
            let (l, rr, m, s) = (acc[1], acc[2], acc[3], acc[4]);
            let t = if r.mono {
                let a = st.0.mul(l.0).mul(m.0);
                let d = st.1.mul(l.1).mul(m.1);
                M2::diag(ratio(a, d), Cx(0.0, 0.0))
            } else {
                // IIR order: left / right stage first, then mid / side: D = Dms * Dlr, so the
                // correction is A * D^-1 = Ams * Clr * Dms^-1 = (Ams Dms^-1) (Dms Clr Dms^-1),
                // written in the mid / side basis with ratios that go to 1 where both are ~0
                let cl = ratio(st.0, st.1).mul(ratio(l.0, l.1));
                let cr = ratio(st.0, st.1).mul(ratio(rr.0, rr.1));
                let (rm, rs) = (ratio(m.0, m.1), ratio(s.0, s.1));
                let p = cl.add(cr).half();
                let q = cl.sub(cr).half();
                let ms = [rm.mul(p), rm.mul(q).mul(ratio(m.1, s.1)), rs.mul(q).mul(ratio(s.1, m.1)), rs.mul(p)];
                from_ms(ms)
            };
            // delay by n / 2 (linear phase centre / natural phase look-ahead)
            let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
            for e in 0..4 {
                spec[e][k] = Cx(t.0[e].0 * sign, t.0[e].1 * sign);
            }
        }
        // to time, window, partition
        let p = r.p;
        let nparts = n.div_ceil(p);
        let inv = self.planner.plan_fft_inverse(n);
        let fwd = self.planner.plan_fft_forward(2 * p);
        let mut freq = inv.make_input_vec();
        let mut time = inv.make_output_vec();
        let mut buf = fwd.make_input_vec();
        let mut part = fwd.make_output_vec();
        let mut h: [Vec<C32>; 4] = Default::default();
        let win: Vec<f32> = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / n as f64).cos()) as f32).collect();
        for e in 0..4 {
            if !used[e] {
                continue;
            }
            for k in 0..bins {
                freq[k] = C32::new(spec[e][k].0 as f32, spec[e][k].1 as f32);
            }
            freq[0].im = 0.0;
            freq[bins - 1].im = 0.0;
            let _ = inv.process(&mut freq, &mut time);
            let g = 1.0 / (n as f32 * 2.0 * p as f32);
            for (v, w) in time.iter_mut().zip(win.iter()) {
                *v *= w * g;
            }
            let mut out = Vec::with_capacity(nparts * (p + 1));
            for i in 0..nparts {
                buf.iter_mut().for_each(|v| *v = 0.0);
                let lo = i * p;
                let hi = ((i + 1) * p).min(n);
                buf[..hi - lo].copy_from_slice(&time[lo..hi]);
                let _ = fwd.process(&mut buf, &mut part);
                out.extend_from_slice(&part);
            }
            h[e] = out;
        }
        Box::new(Kernel { kind: r.kind, p, nparts, h, used })
    }
}

// ------------------------------------------------------------------ worker

struct Slots {
    req: Mutex<(bool, Option<Req>)>,
    cv: Condvar,
    done: Mutex<Option<Box<Kernel>>>,
    recycle: Mutex<Option<Box<Kernel>>>,
    stop: AtomicBool,
}

/// Designs kernels on its own thread. The audio thread only ever `try_lock`s.
pub struct Worker {
    slots: Arc<Slots>,
    th: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn new() -> Self {
        let slots = Arc::new(Slots { req: Mutex::new((false, None)), cv: Condvar::new(), done: Mutex::new(None), recycle: Mutex::new(None), stop: AtomicBool::new(false) });
        let s2 = slots.clone();
        let th = std::thread::Builder::new()
            .name("nyashkeq-kernels".into())
            .spawn(move || {
                let mut des = Designer::default();
                loop {
                    let req = {
                        let mut g = s2.req.lock().unwrap();
                        if !g.0 {
                            g = s2.cv.wait_timeout(g, Duration::from_millis(50)).unwrap().0;
                        }
                        if s2.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        if g.0 {
                            g.0 = false;
                            g.1
                        } else {
                            None
                        }
                    };
                    // old kernels are freed here, not on the audio thread
                    drop(s2.recycle.lock().unwrap().take());
                    if let Some(r) = req {
                        let k = des.build(&r);
                        let old = s2.done.lock().unwrap().replace(k);
                        drop(old);
                    }
                }
            })
            .ok();
        Self { slots, th }
    }

    /// post a request (latest wins); false when the lock was busy (try again next block)
    pub fn post(&self, r: &Req) -> bool {
        if let Ok(mut g) = self.slots.req.try_lock() {
            g.0 = true;
            g.1 = Some(*r);
            drop(g);
            self.slots.cv.notify_one();
            true
        } else {
            false
        }
    }

    /// take a finished kernel if there is one
    pub fn take(&self) -> Option<Box<Kernel>> {
        self.slots.done.try_lock().ok().and_then(|mut g| g.take())
    }

    /// hand a kernel back to be freed by the worker; returns it when the slot is busy
    pub fn recycle(&self, k: Box<Kernel>) -> Option<Box<Kernel>> {
        match self.slots.recycle.try_lock() {
            Ok(mut g) if g.is_none() => {
                *g = Some(k);
                None
            }
            _ => Some(k),
        }
    }
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.slots.stop.store(true, Ordering::Relaxed);
        self.slots.cv.notify_all();
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }
}

// ------------------------------------------------------------------ convolution

/// Uniformly partitioned overlap-save convolution, 2 inputs x 2 outputs, with a crossfade when
/// the kernel changes. Latency: one partition (p samples) on top of the kernel's own delay.
pub struct Conv {
    p: usize,
    max_parts: usize,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    sf: Vec<C32>,
    si: Vec<C32>,
    inb: [Vec<f32>; 2],
    fdl: [Vec<C32>; 2],
    head: usize,
    fill: usize,
    out: [Vec<f32>; 2],
    tbuf: Vec<f32>,
    acc: [Vec<C32>; 2],
    tout: [Vec<f32>; 2],
    pub cur: Option<Box<Kernel>>,
    pub next: Option<Box<Kernel>>,
    /// kernels done with, waiting to go back to the worker
    pub retired: Option<Box<Kernel>>,
}

impl Conv {
    pub fn new(p: usize, max_len: usize) -> Self {
        let mut pl = RealFftPlanner::<f32>::new();
        let fwd = pl.plan_fft_forward(2 * p);
        let inv = pl.plan_fft_inverse(2 * p);
        let max_parts = max_len.div_ceil(p);
        Self {
            p,
            max_parts,
            sf: fwd.make_scratch_vec(),
            si: inv.make_scratch_vec(),
            fwd,
            inv,
            inb: [vec![0.0; 2 * p], vec![0.0; 2 * p]],
            fdl: [vec![C32::new(0.0, 0.0); max_parts * (p + 1)], vec![C32::new(0.0, 0.0); max_parts * (p + 1)]],
            head: 0,
            fill: 0,
            out: [vec![0.0; p], vec![0.0; p]],
            tbuf: vec![0.0; 2 * p],
            acc: [vec![C32::new(0.0, 0.0); p + 1], vec![C32::new(0.0, 0.0); p + 1]],
            tout: [vec![0.0; 2 * p], vec![0.0; 2 * p]],
            cur: None,
            next: None,
            retired: None,
        }
    }

    pub fn p(&self) -> usize {
        self.p
    }

    pub fn reset(&mut self) {
        for c in 0..2 {
            self.inb[c].iter_mut().for_each(|v| *v = 0.0);
            self.fdl[c].iter_mut().for_each(|v| *v = C32::new(0.0, 0.0));
            self.out[c].iter_mut().for_each(|v| *v = 0.0);
        }
        self.fill = 0;
        self.head = 0;
    }

    /// queue a new kernel (it fades in over the next block)
    pub fn set_kernel(&mut self, k: Box<Kernel>) {
        if k.p != self.p || k.nparts > self.max_parts {
            self.retire(k);
            return;
        }
        if self.cur.is_none() {
            self.cur = Some(k);
        } else if let Some(old) = self.next.replace(k) {
            self.retire(old);
        }
    }

    fn retire(&mut self, k: Box<Kernel>) {
        // at most one waiting: if there already is one the worker has not picked up, this one
        // has to be freed here (rare)
        if self.retired.is_none() {
            self.retired = Some(k);
        } else {
            drop(k);
        }
    }

    pub fn has_kernel(&self) -> bool {
        self.cur.is_some()
    }

    #[inline]
    pub fn tick(&mut self, x: [f32; 2]) -> [f32; 2] {
        let i = self.fill;
        let y = [self.out[0][i], self.out[1][i]];
        self.inb[0][self.p + i] = x[0];
        self.inb[1][self.p + i] = x[1];
        self.fill += 1;
        if self.fill == self.p {
            self.fill = 0;
            self.block();
        }
        y
    }

    fn block(&mut self) {
        let p = self.p;
        let w = p + 1;
        let Some(cur) = self.cur.as_ref() else {
            for c in 0..2 {
                self.out[c].iter_mut().for_each(|v| *v = 0.0);
                self.inb[c].copy_within(p..2 * p, 0);
            }
            return;
        };
        let nin = if cur.used[1] || cur.used[3] { 2 } else { 1 };
        // input spectra into the frequency-domain delay line
        for c in 0..nin {
            self.tbuf.copy_from_slice(&self.inb[c]);
            let dst = &mut self.fdl[c][self.head * w..(self.head + 1) * w];
            let _ = self.fwd.process_with_scratch(&mut self.tbuf, dst, &mut self.sf);
            self.inb[c].copy_within(p..2 * p, 0);
        }
        for c in nin..2 {
            self.inb[c].copy_within(p..2 * p, 0);
        }
        let run = |k: &Kernel, acc: &mut [Vec<C32>; 2], tout: &mut [Vec<f32>; 2], fdl: &[Vec<C32>; 2], head: usize, maxp: usize, inv: &Arc<dyn ComplexToReal<f32>>, si: &mut Vec<C32>| {
            for o in 0..2 {
                let a = &mut acc[o];
                a.iter_mut().for_each(|v| *v = C32::new(0.0, 0.0));
                let mut any = false;
                for i in 0..2 {
                    let e = o * 2 + i;
                    if !k.used[e] {
                        continue;
                    }
                    any = true;
                    for part in 0..k.nparts {
                        let slot = (head + maxp - part) % maxp;
                        let x = &fdl[i][slot * w..(slot + 1) * w];
                        let h = &k.h[e][part * w..(part + 1) * w];
                        for ((av, xv), hv) in a.iter_mut().zip(x.iter()).zip(h.iter()) {
                            *av += xv * hv;
                        }
                    }
                }
                if !any {
                    tout[o].iter_mut().for_each(|v| *v = 0.0);
                    continue;
                }
                a[0].im = 0.0;
                a[p].im = 0.0;
                let _ = inv.process_with_scratch(a, &mut tout[o], si);
            }
        };
        let mono = !cur.used[3];
        run(cur, &mut self.acc, &mut self.tout, &self.fdl, self.head, self.max_parts, &self.inv, &mut self.si);
        for o in 0..2 {
            self.out[o].copy_from_slice(&self.tout[o][p..2 * p]);
        }
        if mono {
            // mono kernel: the second output is unused
            let (a, b) = self.out.split_at_mut(1);
            b[0].copy_from_slice(&a[0]);
        }
        if let Some(nk) = self.next.take() {
            let mono_n = !nk.used[3];
            run(&nk, &mut self.acc, &mut self.tout, &self.fdl, self.head, self.max_parts, &self.inv, &mut self.si);
            for o in 0..2 {
                let src = if mono_n { 0 } else { o };
                for j in 0..p {
                    let t = (j as f32 + 0.5) / p as f32;
                    let g = t * t * (3.0 - 2.0 * t);
                    self.out[o][j] += (self.tout[src][p + j] - self.out[o][j]) * g;
                }
            }
            if let Some(old) = self.cur.replace(nk) {
                self.retire(old);
            }
        }
        self.head = (self.head + 1) % self.max_parts;
    }
}
