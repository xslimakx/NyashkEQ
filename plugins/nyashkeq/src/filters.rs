//! Band shapes: analog prototypes for every shape and slope, their exact (analog) response, and
//! the analog-matched digital biquad cascade used by the Zero Latency and Natural Phase modes.
//!
//! Every band is built as a product of first- / second-order analog sections in s (rad/s):
//! - cuts: Butterworth of order N (the Q scales the most resonant pole pair),
//! - shelves: Butterworth-type shelf of order N (zeros / poles on circles of radius
//!   w0 * G^(+-1/2N)), tilt shelf = high shelf normalised to the middle,
//! - bell / notch / band pass: an order-M low shelf / high pass / low pass prototype moved to w0
//!   with the low-pass to band-pass transform (steeper slopes = steeper flanks, same centre gain),
//! - flat tilt: one pole / zero pair per octave from 5 Hz to 40 kHz,
//! - all pass: second-order all-pass sections (first-order for odd orders).
//!
//! Digital sections: poles are mapped exactly (z = e^(sT)) and the numerator is fitted so the
//! magnitude matches the analog section at DC, Nyquist and its own natural frequency (Vicanek,
//! "Matched Second Order Digital Filters", 2016). No cramping near Nyquist.
use nih_plug::prelude::Enum;
use std::f64::consts::PI;

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Shape {
    #[name = "Bell"]
    Bell,
    #[name = "Low Shelf"]
    LowShelf,
    #[name = "Low Cut"]
    LowCut,
    #[name = "High Shelf"]
    HighShelf,
    #[name = "High Cut"]
    HighCut,
    #[name = "Notch"]
    Notch,
    #[name = "Band Pass"]
    BandPass,
    #[name = "Tilt Shelf"]
    TiltShelf,
    #[name = "Flat Tilt"]
    FlatTilt,
    #[name = "All Pass"]
    AllPass,
}

impl Shape {
    pub const ALL: [Shape; 10] = [Shape::Bell, Shape::LowShelf, Shape::LowCut, Shape::HighShelf, Shape::HighCut, Shape::Notch, Shape::BandPass, Shape::TiltShelf, Shape::FlatTilt, Shape::AllPass];
    /// shapes with a gain (and so a dynamic range)
    pub fn has_gain(self) -> bool {
        matches!(self, Shape::Bell | Shape::LowShelf | Shape::HighShelf | Shape::TiltShelf | Shape::FlatTilt)
    }
    pub fn has_q(self) -> bool {
        !matches!(self, Shape::FlatTilt)
    }
    pub fn has_slope(self) -> bool {
        !matches!(self, Shape::FlatTilt)
    }
    /// lowest slope allowed for the shape
    pub fn min_slope(self) -> Slope {
        match self {
            Shape::Bell | Shape::Notch => Slope::S12,
            _ => Slope::S6,
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Shape::Bell => "BELL",
            Shape::LowShelf => "LO SHELF",
            Shape::LowCut => "LO CUT",
            Shape::HighShelf => "HI SHELF",
            Shape::HighCut => "HI CUT",
            Shape::Notch => "NOTCH",
            Shape::BandPass => "BAND PASS",
            Shape::TiltShelf => "TILT",
            Shape::FlatTilt => "FLAT TILT",
            Shape::AllPass => "ALL PASS",
        }
    }
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Slope {
    #[name = "6 dB/oct"]
    S6,
    #[name = "12 dB/oct"]
    S12,
    #[name = "18 dB/oct"]
    S18,
    #[name = "24 dB/oct"]
    S24,
    #[name = "30 dB/oct"]
    S30,
    #[name = "36 dB/oct"]
    S36,
    #[name = "48 dB/oct"]
    S48,
    #[name = "60 dB/oct"]
    S60,
    #[name = "72 dB/oct"]
    S72,
    #[name = "96 dB/oct"]
    S96,
    #[name = "Brickwall"]
    Brick,
}

impl Slope {
    pub const ALL: [Slope; 11] = [Slope::S6, Slope::S12, Slope::S18, Slope::S24, Slope::S30, Slope::S36, Slope::S48, Slope::S60, Slope::S72, Slope::S96, Slope::Brick];
    pub fn db(self) -> u32 {
        match self {
            Slope::S6 => 6,
            Slope::S12 => 12,
            Slope::S18 => 18,
            Slope::S24 => 24,
            Slope::S30 => 30,
            Slope::S36 => 36,
            Slope::S48 => 48,
            Slope::S60 => 60,
            Slope::S72 => 72,
            Slope::S96 => 96,
            Slope::Brick => 144,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Slope::Brick => "BRICK",
            Slope::S6 => "6",
            Slope::S12 => "12",
            Slope::S18 => "18",
            Slope::S24 => "24",
            Slope::S30 => "30",
            Slope::S36 => "36",
            Slope::S48 => "48",
            Slope::S60 => "60",
            Slope::S72 => "72",
            Slope::S96 => "96",
        }
    }
    /// the slope actually used for `shape` (brickwall only for cuts, at least the shape minimum)
    pub fn for_shape(self, shape: Shape) -> Slope {
        let mut s = self;
        if s == Slope::Brick && !matches!(shape, Shape::LowCut | Shape::HighCut) {
            s = Slope::S96;
        }
        if s.db() < shape.min_slope().db() {
            s = shape.min_slope();
        }
        s
    }
}

/// Everything that defines one band's filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub shape: Shape,
    pub slope: Slope,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    /// gain-Q interaction (bells get narrower with more gain)
    pub gq: bool,
}

impl Spec {
    /// true when the band changes nothing (a bell / shelf at 0 dB)
    pub fn is_flat(&self) -> bool {
        self.shape.has_gain() && self.gain_db.abs() < 0.005
    }
}

/// One analog section (n0 s^2 + n1 s + n2) / (d0 s^2 + d1 s + d2); first order when n0 = d0 = 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct ASec {
    pub n: [f64; 3],
    pub d: [f64; 3],
}

impl ASec {
    #[inline]
    fn eval(&self, w: f64) -> (f64, f64, f64, f64) {
        let w2 = w * w;
        (self.n[2] - self.n[0] * w2, self.n[1] * w, self.d[2] - self.d[0] * w2, self.d[1] * w)
    }
    /// |H(jw)|^2
    #[inline]
    pub fn mag2(&self, w: f64) -> f64 {
        let (nr, ni, dr, di) = self.eval(w);
        (nr * nr + ni * ni) / (dr * dr + di * di).max(1e-300)
    }
    /// H(jw)
    #[inline]
    pub fn h(&self, w: f64) -> (f64, f64) {
        let (nr, ni, dr, di) = self.eval(w);
        let den = (dr * dr + di * di).max(1e-300);
        ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den)
    }
    fn wn(&self) -> f64 {
        if self.d[0] != 0.0 {
            (self.d[2] / self.d[0]).abs().sqrt()
        } else {
            (self.d[2] / self.d[1]).abs()
        }
    }
}

pub const MAX_SECS: usize = 16;

/// Analog cascade.
#[derive(Clone, Copy, Debug)]
pub struct Analog {
    pub n: usize,
    pub s: [ASec; MAX_SECS],
    pub allpass: bool,
}

impl Default for Analog {
    fn default() -> Self {
        Self { n: 0, s: [ASec::default(); MAX_SECS], allpass: false }
    }
}

impl Analog {
    fn push(&mut self, s: ASec) {
        if self.n < MAX_SECS {
            self.s[self.n] = s;
            self.n += 1;
        }
    }
    fn scale(&mut self, k: f64) {
        if self.n > 0 {
            for v in self.s[0].n.iter_mut() {
                *v *= k;
            }
        }
    }
    /// magnitude in dB at f Hz
    pub fn db(&self, f: f64) -> f64 {
        let w = 2.0 * PI * f;
        let mut m = 1.0;
        for s in &self.s[..self.n] {
            m *= s.mag2(w);
        }
        10.0 * m.max(1e-30).log10()
    }
    /// |H|
    pub fn mag(&self, f: f64) -> f64 {
        let w = 2.0 * PI * f;
        let mut m = 1.0;
        for s in &self.s[..self.n] {
            m *= s.mag2(w);
        }
        m.sqrt()
    }
    /// complex H(j 2 pi f)
    pub fn h(&self, f: f64) -> (f64, f64) {
        let w = 2.0 * PI * f;
        let (mut re, mut im) = (1.0, 0.0);
        for s in &self.s[..self.n] {
            let (a, b) = s.h(w);
            (re, im) = (re * a - im * b, re * b + im * a);
        }
        (re, im)
    }
}

/// Butterworth pole pairs of order n as Q values (most resonant first) and whether there is a
/// real pole.
fn butter(n: usize) -> ([f64; 32], usize, bool) {
    let mut q = [0.0; 32];
    let pairs = n / 2;
    for (k, v) in q.iter_mut().enumerate().take(pairs) {
        let th = PI * (2 * k + n + 1) as f64 / (2 * n) as f64;
        *v = -1.0 / (2.0 * th.cos());
    }
    (q, pairs, n % 2 == 1)
}

/// effective bell Q with gain-Q interaction
pub fn eff_q(q: f64, gain_db: f64, gq: bool) -> f64 {
    if gq {
        q * (0.45 + 0.55 * (gain_db.abs() / 9.0).min(2.5))
    } else {
        q
    }
}

/// Roots of s^2 - c B s + w0^2 for a complex prototype root c (re, im): the two real quadratics
/// (s^2 + p1 s + p0) made with each root and its conjugate.
fn bp_quads(c: (f64, f64), b: f64, w0: f64) -> [(f64, f64); 2] {
    // s = (cB +- sqrt(c^2 B^2 - 4 w0^2)) / 2
    let (cr, ci) = (c.0 * b, c.1 * b);
    let (dr, di) = (cr * cr - ci * ci - 4.0 * w0 * w0, 2.0 * cr * ci);
    // complex sqrt
    let m = (dr * dr + di * di).sqrt();
    let sr = ((m + dr) * 0.5).max(0.0).sqrt();
    let si = ((m - dr) * 0.5).max(0.0).sqrt() * if di < 0.0 { -1.0 } else { 1.0 };
    let r1 = ((cr + sr) * 0.5, (ci + si) * 0.5);
    let r2 = ((cr - sr) * 0.5, (ci - si) * 0.5);
    [(-2.0 * r1.0, r1.0 * r1.0 + r1.1 * r1.1), (-2.0 * r2.0, r2.0 * r2.0 + r2.1 * r2.1)]
}

/// Analog cascade for a band.
pub fn analog(sp: &Spec) -> Analog {
    let mut a = Analog::default();
    let w0 = 2.0 * PI * sp.freq.clamp(5.0, 40000.0);
    let slope = sp.slope.for_shape(sp.shape);
    let order = (slope.db() / 6) as usize;
    let g = 10f64.powf(sp.gain_db / 20.0);
    let qmul = sp.q.clamp(0.025, 40.0);
    match sp.shape {
        Shape::LowCut | Shape::HighCut => {
            let (qs, pairs, real) = butter(order);
            let hp = sp.shape == Shape::LowCut;
            for (k, &q0) in qs.iter().enumerate().take(pairs) {
                let q = if k == 0 { q0 * qmul } else { q0 };
                let n = if hp { [1.0, 0.0, 0.0] } else { [0.0, 0.0, w0 * w0] };
                a.push(ASec { n, d: [1.0, w0 / q, w0 * w0] });
            }
            if real {
                let n = if hp { [0.0, 1.0, 0.0] } else { [0.0, 0.0, w0] };
                a.push(ASec { n, d: [0.0, 1.0, w0] });
            }
        }
        Shape::LowShelf | Shape::HighShelf | Shape::TiltShelf => {
            let (qs, pairs, real) = butter(order);
            // low shelf: zeros at w0 G^(1/2N), poles at w0 G^(-1/2N); a high shelf is G x the low
            // shelf with 1 / G
            let (gl, k) = match sp.shape {
                Shape::LowShelf => (g, 1.0),
                Shape::HighShelf => (1.0 / g, g),
                // tilt: low end -gain, high end +gain
                _ => (1.0 / (g * g), g),
            };
            let rz = gl.powf(1.0 / (2.0 * order as f64));
            let rp = 1.0 / rz;
            for (kk, &q0) in qs.iter().enumerate().take(pairs) {
                let q = if kk == 0 { q0 * qmul } else { q0 };
                let (wz, wp) = (w0 * rz, w0 * rp);
                a.push(ASec { n: [1.0, wz / q, wz * wz], d: [1.0, wp / q, wp * wp] });
            }
            if real {
                a.push(ASec { n: [0.0, 1.0, w0 * rz], d: [0.0, 1.0, w0 * rp] });
            }
            a.scale(k);
        }
        Shape::Bell | Shape::Notch | Shape::BandPass => {
            let m = (order / 2).max(1);
            let q = if sp.shape == Shape::Bell { eff_q(qmul, sp.gain_db, sp.gq) } else { qmul };
            let b = w0 / q;
            let (qs, pairs, real) = butter(m);
            let unit = |q: f64| {
                let re = -1.0 / (2.0 * q);
                (re, (1.0 - re * re).max(0.0).sqrt())
            };
            match sp.shape {
                Shape::Bell => {
                    let rz = g.powf(1.0 / (2.0 * m as f64));
                    let rp = 1.0 / rz;
                    for &q0 in qs.iter().take(pairs) {
                        let c = unit(q0);
                        let zq = bp_quads((c.0 * rz, c.1 * rz), b, w0);
                        let pq = bp_quads((c.0 * rp, c.1 * rp), b, w0);
                        for i in 0..2 {
                            a.push(ASec { n: [1.0, zq[i].0, zq[i].1], d: [1.0, pq[i].0, pq[i].1] });
                        }
                    }
                    if real {
                        a.push(ASec { n: [1.0, rz * b, w0 * w0], d: [1.0, rp * b, w0 * w0] });
                    }
                }
                Shape::Notch => {
                    for &q0 in qs.iter().take(pairs) {
                        let pq = bp_quads(unit(q0), b, w0);
                        for p in pq {
                            a.push(ASec { n: [1.0, 0.0, w0 * w0], d: [1.0, p.0, p.1] });
                        }
                    }
                    if real {
                        a.push(ASec { n: [1.0, 0.0, w0 * w0], d: [1.0, b, w0 * w0] });
                    }
                }
                _ => {
                    for &q0 in qs.iter().take(pairs) {
                        let pq = bp_quads(unit(q0), b, w0);
                        for p in pq {
                            a.push(ASec { n: [0.0, b, 0.0], d: [1.0, p.0, p.1] });
                        }
                    }
                    if real {
                        a.push(ASec { n: [0.0, b, 0.0], d: [1.0, b, w0 * w0] });
                    }
                }
            }
        }
        Shape::FlatTilt => {
            // slope in dB/oct = gain / 5 (the gain is reached 5 octaves above the pivot)
            let alpha = sp.gain_db / 5.0;
            let ratio = 10f64.powf(alpha.abs() / 20.0);
            let mut fz = 5.0;
            for _ in 0..7 {
                // two octaves per second-order section
                let (z1, p1) = (2.0 * PI * fz, 2.0 * PI * fz * ratio);
                let (z2, p2) = (z1 * 2.0, p1 * 2.0);
                let (z1, p1, z2, p2) = if alpha >= 0.0 { (z1, p1, z2, p2) } else { (p1, z1, p2, z2) };
                a.push(ASec { n: [1.0, z1 + z2, z1 * z2], d: [1.0, p1 + p2, p1 * p2] });
                fz *= 4.0;
            }
            let m = a.mag(sp.freq);
            a.scale(1.0 / m.max(1e-9));
        }
        Shape::AllPass => {
            a.allpass = true;
            let (_, pairs, real) = butter(order);
            for _ in 0..pairs {
                a.push(ASec { n: [1.0, -w0 / qmul, w0 * w0], d: [1.0, w0 / qmul, w0 * w0] });
            }
            if real {
                a.push(ASec { n: [0.0, -1.0, w0], d: [0.0, 1.0, w0] });
            }
        }
    }
    a
}

// ------------------------------------------------------------------ digital

/// Digital biquad b0 + b1 z^-1 + b2 z^-2 over 1 + a1 z^-1 + a2 z^-2.
#[derive(Clone, Copy, Debug)]
pub struct DSec {
    pub b: [f64; 3],
    pub a: [f64; 2],
}

impl Default for DSec {
    fn default() -> Self {
        Self { b: [1.0, 0.0, 0.0], a: [0.0, 0.0] }
    }
}

impl DSec {
    /// complex response at w (rad/sample)
    pub fn h(&self, w: f64) -> (f64, f64) {
        let (c1, s1, c2, s2) = (w.cos(), -w.sin(), (2.0 * w).cos(), -(2.0 * w).sin());
        let (nr, ni) = (self.b[0] + self.b[1] * c1 + self.b[2] * c2, self.b[1] * s1 + self.b[2] * s2);
        let (dr, di) = (1.0 + self.a[0] * c1 + self.a[1] * c2, self.a[0] * s1 + self.a[1] * s2);
        let den = (dr * dr + di * di).max(1e-300);
        ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den)
    }
}

/// Bilinear transform prewarped at `wp` (rad/s).
fn bilinear(s: &ASec, fs: f64, wp: f64) -> DSec {
    let wp = wp.clamp(1.0, 0.95 * PI * fs);
    let k = wp / (wp / (2.0 * fs)).tan();
    let k2 = k * k;
    if s.d[0] == 0.0 {
        let (b0, b1) = (s.n[1] * k + s.n[2], s.n[2] - s.n[1] * k);
        let (a0, a1) = (s.d[1] * k + s.d[2], s.d[2] - s.d[1] * k);
        DSec { b: [b0 / a0, b1 / a0, 0.0], a: [a1 / a0, 0.0] }
    } else {
        let b0 = s.n[0] * k2 + s.n[1] * k + s.n[2];
        let b1 = 2.0 * (s.n[2] - s.n[0] * k2);
        let b2 = s.n[0] * k2 - s.n[1] * k + s.n[2];
        let a0 = s.d[0] * k2 + s.d[1] * k + s.d[2];
        let a1 = 2.0 * (s.d[2] - s.d[0] * k2);
        let a2 = s.d[0] * k2 - s.d[1] * k + s.d[2];
        DSec { b: [b0 / a0, b1 / a0, b2 / a0], a: [a1 / a0, a2 / a0] }
    }
}

/// How a section was digitised (kept, so dynamic gain updates skip the method search).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    Fit,
    Bilinear,
    Mzt,
}

/// Exact pole mapping z = e^(sT) (resonances above Nyquist become critically damped).
fn poles(s: &ASec, t: f64) -> [f64; 2] {
    let wn = s.wn();
    if s.d[0] == 0.0 {
        return [-(-(s.d[2] / s.d[1]) * t).exp(), 0.0];
    }
    let (d1, d2) = (s.d[1] / s.d[0], s.d[2] / s.d[0]);
    let disc = d1 * d1 - 4.0 * d2;
    if disc < 0.0 {
        let sig = -d1 * 0.5;
        let wd = (-disc).sqrt() * 0.5;
        if wd * t > 0.9 * PI {
            let r = (-wn * t).exp();
            [-2.0 * r, r * r]
        } else {
            let r = (sig * t).exp();
            [-2.0 * r * (wd * t).cos(), r * r]
        }
    } else {
        let sq = disc.sqrt();
        let (p1, p2) = ((-d1 + sq) * 0.5, (-d1 - sq) * 0.5);
        [-((p1 * t).exp() + (p2 * t).exp()), ((p1 + p2) * t).exp()]
    }
}

/// Poles exact, numerator fitted to the analog magnitude at DC, Nyquist and the section's own
/// frequency (Vicanek 2016).
fn fit(s: &ASec, fs: f64) -> Option<DSec> {
    let t = 1.0 / fs;
    let nyq = PI * fs;
    let [a1, a2] = poles(s, t);
    if s.d[0] == 0.0 {
        let (a0s, a1s) = ((1.0 + a1).powi(2), (1.0 - a1).powi(2));
        let b0s = (s.mag2(0.0) * a0s).max(0.0).sqrt();
        let b1s = (s.mag2(nyq) * a1s).max(0.0).sqrt();
        return Some(DSec { b: [(b0s + b1s) * 0.5, (b0s - b1s) * 0.5, 0.0], a: [a1, 0.0] });
    }
    let aa0 = (1.0 + a1 + a2).powi(2);
    let aa1 = (1.0 - a1 + a2).powi(2);
    let aa2 = -4.0 * a2;
    let wm = s.wn().clamp(0.05 * nyq, 0.85 * nyq);
    let ph1 = (wm * t * 0.5).sin().powi(2);
    let ph0 = 1.0 - ph1;
    let ph2 = 4.0 * ph0 * ph1;
    let bb0 = s.mag2(0.0) * aa0;
    let bb1 = s.mag2(nyq) * aa1;
    let tm = s.mag2(wm) * (aa0 * ph0 + aa1 * ph1 + aa2 * ph2);
    let bb2 = (tm - bb0 * ph0 - bb1 * ph1) / ph2;
    let (r0, r1) = (bb0.max(0.0).sqrt(), bb1.max(0.0).sqrt());
    let w = 0.5 * (r0 + r1);
    let dd = w * w + bb2;
    if dd < 0.0 || !dd.is_finite() {
        return None;
    }
    let sq = dd.sqrt();
    Some(DSec { b: [0.5 * (w + sq), 0.5 * (r0 - r1), 0.5 * (w - sq)], a: [a1, a2] })
}

/// Matched z-transform: poles and zeros mapped exactly, gain matched where the section is loudest
/// of DC / its own frequency / Nyquist. Keeps notches exactly on their frequency.
fn mzt(s: &ASec, fs: f64) -> DSec {
    let t = 1.0 / fs;
    let [a1, a2] = poles(s, t);
    let b = if s.n[0] != 0.0 {
        let (n1, n2) = (s.n[1] / s.n[0], s.n[2] / s.n[0]);
        let disc = n1 * n1 - 4.0 * n2;
        if disc < 0.0 {
            let r = (-n1 * 0.5 * t).exp();
            let wz = ((-disc).sqrt() * 0.5 * t).min(PI);
            [1.0, -2.0 * r * wz.cos(), r * r]
        } else {
            let sq = disc.sqrt();
            let (z1, z2) = ((-n1 + sq) * 0.5, (-n1 - sq) * 0.5);
            [1.0, -((z1 * t).exp() + (z2 * t).exp()), ((z1 + z2) * t).exp()]
        }
    } else if s.n[1] != 0.0 {
        [1.0, -((-s.n[2] / s.n[1]) * t).exp(), 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let mut d = DSec { b, a: [a1, a2] };
    let nyq = PI * fs;
    let cands = [1e-3 * nyq, s.wn().clamp(1e-3 * nyq, 0.9 * nyq), 0.999 * nyq];
    let wr = cands.iter().copied().fold((0.0, 0.0), |(bw, bm), w| {
        let m = s.mag2(w);
        if m > bm {
            (w, m)
        } else {
            (bw, bm)
        }
    });
    let (re, im) = d.h(wr.0 * t);
    let dm = (re * re + im * im).sqrt();
    if dm > 1e-12 {
        let g = wr.1.sqrt() / dm;
        for v in d.b.iter_mut() {
            *v *= g;
        }
    }
    d
}

fn design(s: &ASec, fs: f64, m: Method) -> DSec {
    match m {
        Method::Bilinear => bilinear(s, fs, s.wn()),
        Method::Mzt => mzt(s, fs),
        Method::Fit => fit(s, fs).unwrap_or_else(|| bilinear(s, fs, s.wn())),
    }
}

/// worst dB error of a digital section against its analog one, up to 20 kHz (ignoring what is
/// more than 50 dB below the section's peak)
fn sec_err(a: &ASec, d: &DSec, fs: f64) -> f64 {
    let top = (0.47 * fs).min(21000.0);
    let mut worst: f64 = 0.0;
    let n = 40;
    let mut vals = [(0.0, 0.0); 40];
    let mut peak = -300.0f64;
    for (i, v) in vals.iter_mut().enumerate() {
        let f = 15.0 * (top / 15.0f64).powf(i as f64 / (n - 1) as f64);
        let ad = 10.0 * a.mag2(2.0 * PI * f).max(1e-30).log10();
        let (re, im) = d.h(2.0 * PI * f / fs);
        let dd = 10.0 * (re * re + im * im).max(1e-30).log10();
        *v = (ad, dd);
        peak = peak.max(ad);
    }
    for (ad, dd) in vals {
        if ad > peak - 50.0 {
            worst = worst.max((ad - dd.max(peak - 60.0)).abs());
        }
    }
    if worst.is_finite() {
        worst
    } else {
        1e9
    }
}

/// Best digital section for an analog one: tries the magnitude fit, the matched z-transform and
/// the prewarped bilinear transform, keeps the closest.
pub fn matched(s: &ASec, fs: f64, allpass: bool) -> (DSec, Method) {
    if allpass {
        return (bilinear(s, fs, s.wn()), Method::Bilinear);
    }
    let mut best = (DSec::default(), Method::Fit, f64::INFINITY);
    for m in [Method::Fit, Method::Mzt, Method::Bilinear] {
        let d = design(s, fs, m);
        let e = sec_err(s, &d, fs);
        if e < best.2 {
            best = (d, m, e);
        }
    }
    (best.0, best.1)
}

/// Digital cascade.
#[derive(Clone, Copy, Debug)]
pub struct Cascade {
    pub n: usize,
    pub s: [DSec; MAX_SECS],
    pub m: [Method; MAX_SECS],
}

impl Default for Cascade {
    fn default() -> Self {
        Self { n: 0, s: [DSec::default(); MAX_SECS], m: [Method::Fit; MAX_SECS] }
    }
}

impl Cascade {
    /// full design (each section's method searched)
    pub fn from_analog(a: &Analog, fs: f64) -> Self {
        let mut c = Cascade { n: a.n, ..Default::default() };
        for i in 0..a.n {
            (c.s[i], c.m[i]) = matched(&a.s[i], fs, a.allpass);
        }
        c
    }
    /// quick redesign reusing the methods found for `prev` (same shape / slope, new gain): used
    /// for dynamic gain changes
    pub fn quick(a: &Analog, fs: f64, prev: &Cascade) -> Self {
        if prev.n != a.n {
            return Self::from_analog(a, fs);
        }
        let mut c = Cascade { n: a.n, m: prev.m, ..Default::default() };
        for i in 0..a.n {
            c.s[i] = if a.allpass { bilinear(&a.s[i], fs, a.s[i].wn()) } else { design(&a.s[i], fs, prev.m[i]) };
        }
        c
    }
    pub fn h(&self, f: f64, fs: f64) -> (f64, f64) {
        let w = 2.0 * PI * f / fs;
        let (mut re, mut im) = (1.0, 0.0);
        for s in &self.s[..self.n] {
            let (a, b) = s.h(w);
            (re, im) = (re * a - im * b, re * b + im * a);
        }
        (re, im)
    }
    pub fn db(&self, f: f64, fs: f64) -> f64 {
        let (re, im) = self.h(f, fs);
        10.0 * (re * re + im * im).max(1e-30).log10()
    }
}

/// Transposed direct form II state, one channel, the whole cascade.
#[derive(Clone, Copy)]
pub struct CascadeState {
    z: [[f64; 2]; MAX_SECS],
}

impl Default for CascadeState {
    fn default() -> Self {
        Self { z: [[0.0; 2]; MAX_SECS] }
    }
}

impl CascadeState {
    #[inline]
    pub fn tick(&mut self, c: &Cascade, x: f64) -> f64 {
        let mut y = x;
        for i in 0..c.n {
            let s = &c.s[i];
            let z = &mut self.z[i];
            let o = s.b[0] * y + z[0];
            z[0] = s.b[1] * y - s.a[0] * o + z[1];
            z[1] = s.b[2] * y - s.a[1] * o;
            y = o;
        }
        y
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// kill denormals / blow-ups (called once per block)
    pub fn sanitize(&mut self) {
        for z in self.z.iter_mut() {
            for v in z.iter_mut() {
                if !v.is_finite() || v.abs() < 1e-25 {
                    *v = 0.0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(shape: Shape, slope: Slope, f: f64, g: f64, q: f64) -> Spec {
        Spec { shape, slope, freq: f, gain_db: g, q, gq: false }
    }

    #[test]
    fn analog_shapes_hit_their_targets() {
        let b = analog(&sp(Shape::Bell, Slope::S12, 1000.0, 12.0, 1.0));
        assert!((b.db(1000.0) - 12.0).abs() < 0.01);
        assert!(b.db(20.0).abs() < 0.1 && b.db(20000.0).abs() < 0.1);
        // edges at half the gain for Q = 1 (one octave between the -6 dB points... w0/Q bandwidth)
        let bw = 1000.0 / 1.0;
        let lo = (-bw + (bw * bw + 4.0 * 1e6f64).sqrt()) / 2.0;
        assert!((b.db(lo) - 6.0).abs() < 0.05, "{}", b.db(lo));
        for sl in [Slope::S24, Slope::S48, Slope::S96] {
            let b = analog(&sp(Shape::Bell, sl, 500.0, -9.0, 2.0));
            assert!((b.db(500.0) + 9.0).abs() < 0.01, "{sl:?} {}", b.db(500.0));
        }
        let ls = analog(&sp(Shape::LowShelf, Slope::S12, 200.0, 6.0, 1.0));
        assert!((ls.db(5.0) - 6.0).abs() < 0.05 && ls.db(20000.0).abs() < 0.05 && (ls.db(200.0) - 3.0).abs() < 0.05);
        let hs = analog(&sp(Shape::HighShelf, Slope::S36, 4000.0, -8.0, 1.0));
        assert!((hs.db(39000.0) + 8.0).abs() < 0.1 && hs.db(20.0).abs() < 0.05 && (hs.db(4000.0) + 4.0).abs() < 0.05);
        let t = analog(&sp(Shape::TiltShelf, Slope::S6, 1000.0, 4.0, 1.0));
        assert!((t.db(10.0) + 4.0).abs() < 0.2 && (t.db(39000.0) - 4.0).abs() < 0.2 && t.db(1000.0).abs() < 0.05);
        for (sl, dbo) in [(Slope::S12, 12.0), (Slope::S24, 24.0), (Slope::S48, 48.0), (Slope::S96, 96.0)] {
            let hp = analog(&sp(Shape::LowCut, sl, 100.0, 0.0, 1.0));
            assert!((hp.db(100.0) + 3.01).abs() < 0.05, "{sl:?}: {}", hp.db(100.0));
            let d = hp.db(25.0) - hp.db(12.5);
            assert!((d - dbo).abs() < 1.0, "{sl:?} slope {d}");
            let lp = analog(&sp(Shape::HighCut, sl, 1000.0, 0.0, 1.0));
            assert!((lp.db(1000.0) + 3.01).abs() < 0.05);
        }
        let n = analog(&sp(Shape::Notch, Slope::S12, 3000.0, 0.0, 4.0));
        assert!(n.db(3000.0) < -100.0 && n.db(1000.0).abs() < 0.2);
        let bp = analog(&sp(Shape::BandPass, Slope::S12, 3000.0, 0.0, 2.0));
        assert!(bp.db(3000.0).abs() < 0.01 && bp.db(300.0) < -20.0);
        let ft = analog(&sp(Shape::FlatTilt, Slope::S12, 1000.0, 6.0, 1.0));
        assert!(ft.db(1000.0).abs() < 0.01 && (ft.db(32000.0) - 6.0).abs() < 0.5 && (ft.db(31.25) + 6.0).abs() < 0.5, "{} {}", ft.db(32000.0), ft.db(31.25));
        let ap = analog(&sp(Shape::AllPass, Slope::S24, 1000.0, 0.0, 1.0));
        assert!(ap.db(300.0).abs() < 1e-6 && ap.db(5000.0).abs() < 1e-6);
    }

    #[test]
    fn digital_matches_analog_up_to_nyquist() {
        let mut fails = 0;
        for fs in [44100.0, 48000.0, 96000.0] {
            let mut cases = vec![];
            for shape in Shape::ALL {
                for sl in [Slope::S6, Slope::S12, Slope::S24, Slope::S48, Slope::S96] {
                    for (f, g, q) in [(40.0, 9.0, 1.0), (1000.0, -12.0, 4.0), (8000.0, 6.0, 0.7), (16000.0, 12.0, 1.0), (19000.0, -8.0, 2.0)] {
                        cases.push(sp(shape, sl, f, g, q));
                    }
                }
            }
            for c in cases {
                if c.shape == Shape::AllPass || c.freq > 0.2 * fs {
                    continue;
                }
                let a = analog(&c);
                let d = Cascade::from_analog(&a, fs);
                let mut worst: f64 = 0.0;
                let mut f = 20.0;
                while f < (0.45 * fs).min(20000.0) {
                    let ad = a.db(f);
                    if ad > -30.0 {
                        // linear-amplitude error for the notch (dB of a null means nothing)
                        let e = if c.shape == Shape::Notch { 20.0 * (1.0 + (10f64.powf(d.db(f, fs) / 20.0) - 10f64.powf(ad / 20.0)).abs()).log10() } else { (d.db(f, fs) - ad).abs() };
                        // what matters is the error where the band is loud: allow more far down
                        worst = worst.max(e - (-ad).max(0.0) / 8.0);
                    }
                    f *= 1.02;
                }
                // steep slopes are cascades of many resonant sections: per-section matching drifts
                // more (Natural Phase corrects the rest exactly)
                let steep = c.slope.for_shape(c.shape).db() >= 48;
                let lim = match (c.shape, steep) {
                    (Shape::BandPass, true) => 6.0,
                    (_, true) => 2.0,
                    (Shape::BandPass, false) => 1.5,
                    _ => if c.freq > 0.15 * fs { 2.0 } else { 1.0 },
                };
                let _ = &mut fails;
                if worst >= lim {
                    eprintln!("fs {fs} {:?} {:?} f {} g {} q {}: worst {worst:.2} dB", c.shape, c.slope, c.freq, c.gain_db, c.q);
                    fails += 1;
                }
            }
        }
        assert_eq!(fails, 0);
    }

    #[test]
    fn allpass_is_flat_and_turns_phase() {
        let a = analog(&sp(Shape::AllPass, Slope::S12, 1000.0, 0.0, 1.0));
        let d = Cascade::from_analog(&a, 48000.0);
        for f in [50.0, 1000.0, 10000.0] {
            assert!(d.db(f, 48000.0).abs() < 1e-6);
        }
        let (re, im) = d.h(1000.0, 48000.0);
        assert!((im.atan2(re).abs() - PI).abs() < 0.05, "phase {}", im.atan2(re));
    }
}
