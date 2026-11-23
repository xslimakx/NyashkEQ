//! Spectral dynamics: instead of moving a whole band's gain, every FFT bin inside the band is
//! compressed (or expanded) on its own when it goes over the threshold, so only the frequencies
//! that actually stick out are touched (resonances, harshness) and the rest stays as it is.
//!
//! STFT, sqrt-Hann analysis and synthesis windows, 75 % overlap. Latency = frame length.
//! Per band:
//! - weight per bin: the band's own shape (a bell weighs its centre 1 and its skirts less),
//! - level per bin (optionally tilted +3 dB/oct around 1 kHz so the highs trigger as easily),
//! - density: 100 % compares each bin with the threshold only (like a normal dynamic band),
//!   0 % also with the local average of its neighbours, so only peaks above their surroundings
//!   are processed,
//! - soft knee, range limit, per-bin attack / release.
use crate::conv::{Place, NB};
use crate::filters::{analog, Spec};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

type C32 = Complex<f32>;

/// one spectral band as the engine hands it over each control tick
#[derive(Clone, Copy, Debug)]
pub struct SpBand {
    pub active: bool,
    pub spec: Spec,
    pub place: Place,
    pub range: f64,
    pub thr_auto: bool,
    pub thr: f64,
    pub density: f64,
    pub tilt: bool,
    /// per-frame attack / release coefficients
    pub att: f32,
    pub rel: f32,
}

pub struct Spectral {
    pub n: usize,
    hop: usize,
    fs: f64,
    win: Vec<f32>,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    sf: Vec<C32>,
    si: Vec<C32>,
    stage: [Vec<f32>; 2],
    inb: [Vec<f32>; 2],
    acc: [Vec<f32>; 2],
    outq: [Vec<f32>; 2],
    cnt: usize,
    tbuf: Vec<f32>,
    spec: [Vec<C32>; 2],
    dom: Vec<C32>,
    other: Vec<C32>,
    lvl: Vec<f32>,
    pre: Vec<f64>,
    weight: Vec<Vec<f32>>,
    wkey: [Option<Spec>; NB],
    gain: Vec<Vec<f32>>,
    avg: [f32; NB],
    pub bands: [Option<SpBand>; NB],
    /// level normalisation: a full-scale sine reads 0 dB
    cnorm: f32,
    tiltv: Vec<f32>,
    /// total spectral gain per bin (dB), for the display
    pub show: Vec<f32>,
}

impl Spectral {
    pub fn new(fs: f64) -> Self {
        let sc = ((fs / 48000.0).round().max(1.0) as usize).next_power_of_two().min(4);
        let n = 2048 * sc;
        let hop = n / 4;
        let bins = n / 2 + 1;
        let mut pl = RealFftPlanner::<f32>::new();
        let fwd = pl.plan_fft_forward(n);
        let inv = pl.plan_fft_inverse(n);
        let win: Vec<f32> = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).sqrt() as f32).collect();
        let wsum: f32 = win.iter().sum();
        let tiltv = (0..bins).map(|k| 3.0 * ((k as f64 * fs / n as f64).max(20.0) / 1000.0).log2() as f32).collect();
        Self {
            n,
            hop,
            fs,
            sf: fwd.make_scratch_vec(),
            si: inv.make_scratch_vec(),
            fwd,
            inv,
            win,
            stage: [vec![0.0; hop], vec![0.0; hop]],
            inb: [vec![0.0; n], vec![0.0; n]],
            acc: [vec![0.0; n], vec![0.0; n]],
            outq: [vec![0.0; hop], vec![0.0; hop]],
            cnt: 0,
            tbuf: vec![0.0; n],
            spec: [vec![C32::new(0.0, 0.0); bins], vec![C32::new(0.0, 0.0); bins]],
            dom: vec![C32::new(0.0, 0.0); bins],
            other: vec![C32::new(0.0, 0.0); bins],
            lvl: vec![0.0; bins],
            pre: vec![0.0; bins + 1],
            weight: (0..NB).map(|_| vec![0.0; bins]).collect(),
            wkey: [None; NB],
            gain: (0..NB).map(|_| vec![0.0; bins]).collect(),
            avg: [-60.0; NB],
            bands: [None; NB],
            cnorm: (2.0 / wsum).powi(2),
            tiltv,
            show: vec![0.0; bins],
        }
    }

    pub fn latency(&self) -> usize {
        self.n
    }

    /// per-frame time coefficient for `ms`
    pub fn coef(&self, ms: f64) -> f32 {
        (-(self.hop as f64) / (ms.max(0.1) * 0.001 * self.fs)).exp() as f32
    }

    pub fn reset(&mut self) {
        for c in 0..2 {
            self.inb[c].iter_mut().for_each(|v| *v = 0.0);
            self.acc[c].iter_mut().for_each(|v| *v = 0.0);
            self.outq[c].iter_mut().for_each(|v| *v = 0.0);
        }
        for g in self.gain.iter_mut() {
            g.iter_mut().for_each(|v| *v = 0.0);
        }
        self.show.iter_mut().for_each(|v| *v = 0.0);
        self.cnt = 0;
    }

    #[inline]
    pub fn tick(&mut self, x: [f32; 2], nch: usize) -> [f32; 2] {
        let i = self.cnt;
        let y = [self.outq[0][i], self.outq[1][i]];
        self.stage[0][i] = x[0];
        self.stage[1][i] = x[1];
        self.cnt += 1;
        if self.cnt == self.hop {
            self.cnt = 0;
            self.frame(nch);
        }
        y
    }

    fn update_weights(&mut self) {
        let bins = self.n / 2 + 1;
        for b in 0..NB {
            let Some(sb) = self.bands[b] else { continue };
            // the shape at a reference gain of the band's sign: 1 at the band's full effect
            let mut s = sb.spec;
            s.gain_db = if sb.range < 0.0 { -12.0 } else { 12.0 };
            if self.wkey[b] == Some(s) {
                continue;
            }
            self.wkey[b] = Some(s);
            let a = analog(&s);
            for k in 0..bins {
                let f = (k as f64 * self.fs / self.n as f64).max(1.0);
                self.weight[b][k] = (a.db(f) / s.gain_db).clamp(0.0, 1.0) as f32;
            }
        }
    }

    fn frame(&mut self, nch: usize) {
        let (n, hop) = (self.n, self.hop);
        let bins = n / 2 + 1;
        for c in 0..nch {
            self.inb[c].copy_within(hop..n, 0);
            self.inb[c][n - hop..].copy_from_slice(&self.stage[c]);
            for ((t, x), w) in self.tbuf.iter_mut().zip(self.inb[c].iter()).zip(self.win.iter()) {
                *t = x * w;
            }
            let _ = self.fwd.process_with_scratch(&mut self.tbuf, &mut self.spec[c], &mut self.sf);
        }
        self.update_weights();
        self.show.iter_mut().for_each(|v| *v = 0.0);
        for b in 0..NB {
            let Some(sb) = self.bands[b] else {
                self.gain[b].iter_mut().for_each(|v| *v = 0.0);
                continue;
            };
            if !sb.active {
                // release towards 0 dB
                for g in self.gain[b].iter_mut() {
                    *g *= sb.rel;
                }
                continue;
            }
            // the domain the band works in
            let place = if nch == 1 { Place::Stereo } else { sb.place };
            match place {
                Place::Stereo | Place::Left => self.dom.copy_from_slice(&self.spec[0]),
                Place::Right => self.dom.copy_from_slice(&self.spec[1]),
                Place::Mid | Place::Side => {
                    for k in 0..bins {
                        let (l, r) = (self.spec[0][k], self.spec[1][k]);
                        self.dom[k] = if place == Place::Mid { (l + r) * 0.5 } else { (l - r) * 0.5 };
                        self.other[k] = if place == Place::Mid { (l - r) * 0.5 } else { (l + r) * 0.5 };
                    }
                }
            }
            // level per bin (stereo: the louder channel)
            for k in 0..bins {
                let mut p = self.dom[k].norm_sqr();
                if place == Place::Stereo && nch == 2 {
                    p = p.max(self.spec[1][k].norm_sqr());
                }
                self.lvl[k] = 10.0 * (p * self.cnorm + 1e-14).log10() + if sb.tilt { self.tiltv[k] } else { 0.0 };
            }
            // local average (in power) over about +-1/8 of the bin's frequency
            self.pre[0] = 0.0;
            for k in 0..bins {
                self.pre[k + 1] = self.pre[k] + 10f64.powf(self.lvl[k] as f64 / 10.0);
            }
            // auto threshold: the band's own average level
            let w = &self.weight[b];
            let (mut sp, mut sw) = (0.0f64, 0.0f64);
            for k in 1..bins {
                if w[k] > 0.3 {
                    sp += (self.pre[k + 1] - self.pre[k]) * w[k] as f64;
                    sw += w[k] as f64;
                }
            }
            let band_db = if sw > 0.0 { (10.0 * (sp / sw + 1e-14).log10()) as f32 } else { -140.0 };
            if band_db > -90.0 {
                self.avg[b] += (band_db - self.avg[b]) * 0.02;
            }
            let thr = if sb.thr_auto { self.avg[b] + 6.0 } else { sb.thr as f32 };
            let d = sb.density.clamp(0.0, 1.0) as f32;
            let range = sb.range as f32;
            let g = &mut self.gain[b];
            for k in 1..bins {
                if w[k] <= 0.002 {
                    g[k] *= sb.rel;
                    continue;
                }
                let span = (k / 8).max(1);
                let (lo, hi) = (k.saturating_sub(span), (k + span + 1).min(bins));
                let local = (10.0 * ((self.pre[hi] - self.pre[lo]) / (hi - lo) as f64 + 1e-14).log10()) as f32;
                let over = (self.lvl[k] - thr) - (1.0 - d) * (local - thr).max(0.0);
                let o = if over <= -3.0 {
                    0.0
                } else if over < 3.0 {
                    (over + 3.0).powi(2) / 12.0
                } else {
                    over
                };
                let target = if range < 0.0 { -o.min(-range) } else { o.min(range) } * w[k];
                let c = if target.abs() > g[k].abs() { sb.att } else { sb.rel };
                g[k] = target + (g[k] - target) * c;
            }
            // apply
            for k in 1..bins {
                if g[k].abs() < 1e-3 {
                    continue;
                }
                self.show[k] += g[k];
                let lin = 10f32.powf(g[k] / 20.0);
                match place {
                    Place::Stereo => {
                        self.spec[0][k] *= lin;
                        self.spec[1][k] *= lin;
                    }
                    Place::Left => self.spec[0][k] *= lin,
                    Place::Right => self.spec[1][k] *= lin,
                    Place::Mid | Place::Side => {
                        let a = self.dom[k] * lin;
                        let o = self.other[k];
                        let (m, s) = if place == Place::Mid { (a, o) } else { (o, a) };
                        self.spec[0][k] = m + s;
                        self.spec[1][k] = m - s;
                    }
                }
            }
        }
        let scale = 0.5 / n as f32;
        for c in 0..nch {
            self.spec[c][0].im = 0.0;
            self.spec[c][bins - 1].im = 0.0;
            let _ = self.inv.process_with_scratch(&mut self.spec[c], &mut self.tbuf, &mut self.si);
            for (a, (t, w)) in self.acc[c].iter_mut().zip(self.tbuf.iter().zip(self.win.iter())) {
                *a += t * w * scale;
            }
            self.outq[c].copy_from_slice(&self.acc[c][..hop]);
            self.acc[c].copy_within(hop..n, 0);
            self.acc[c][n - hop..].iter_mut().for_each(|v| *v = 0.0);
        }
    }
}
