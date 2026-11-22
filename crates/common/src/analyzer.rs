//! Live analysis for the displays: a smoothed FFT spectrum read from a `Tap`, and a scrolling
//! waveform (peak history) for level-over-time views.
use crate::{fft::Fft, tap::Tap};
use nih_plug_egui::egui::{epaint::PathStroke, Color32, Painter, Pos2, Rect, Shape, Stroke};

/// Smoothed magnitude spectrum (fast attack, slow release), like a hardware analyser.
pub struct Spectrum {
    fft: Fft,
    frame: Vec<f32>,
    spec: Vec<f32>,
    pub smooth: Vec<f32>,
}

impl Spectrum {
    /// `n` must be a power of two (4096 is a good default at 44.1 / 48 kHz).
    pub fn new(n: usize) -> Self {
        Self { fft: Fft::new(n), frame: vec![0.0; n], spec: vec![-140.0; n / 2], smooth: vec![-140.0; n / 2] }
    }

    pub fn len(&self) -> usize {
        self.frame.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frame.is_empty()
    }

    /// Read the newest samples from `tap` and update the smoothed spectrum. Call once per frame.
    pub fn update(&mut self, tap: &Tap) {
        tap.snapshot(&mut self.frame);
        self.fft.magnitude_db(&self.frame, &mut self.spec);
        for (s, v) in self.smooth.iter_mut().zip(self.spec.iter()) {
            let k = if *v > *s { 0.6 } else { 0.12 };
            *s += (*v - *s) * k;
        }
    }

    /// Spectrum as a polyline over `f_lo..f_hi` (log spaced). Each point takes the loudest bin it covers,
    /// so the top end does not alias. `tilt` dB/oct around 1 kHz (4.5 makes pink noise look flat).
    #[allow(clippy::too_many_arguments)]
    pub fn points(&self, fs: f32, f_lo: f32, f_hi: f32, npts: usize, tilt: f32, fx: impl Fn(f32) -> f32, dy: impl Fn(f32) -> f32) -> Vec<Pos2> {
        let n = self.frame.len() as f32;
        let bin = |f: f32| (f * n / fs).clamp(1.0, n / 2.0 - 1.0);
        let mut out = Vec::with_capacity(npts + 1);
        for i in 0..=npts {
            let f0 = f_lo * (f_hi / f_lo).powf(i as f32 / npts as f32);
            let f1 = f_lo * (f_hi / f_lo).powf((i as f32 + 1.0) / npts as f32);
            let (b0, b1) = (bin(f0), bin(f1));
            let db = if b1 - b0 < 1.0 {
                // between bins: a smooth (cubic) curve through them, not straight segments
                cubic_at(&self.smooth, 0.5 * (b0 + b1))
            } else {
                (b0 as usize..=(b1 as usize).min(self.smooth.len() - 1)).map(|k| self.smooth[k]).fold(-200.0f32, f32::max)
            };
            out.push(Pos2::new(fx(f0), dy(db + tilt * (f0 / 1000.0).log2())));
        }
        // a light [1 2 1] blur: no hard steps where several bins fall on one point
        if out.len() > 2 {
            let ys: Vec<f32> = out.iter().map(|q| q.y).collect();
            for i in 1..out.len() - 1 {
                out[i].y = (ys[i - 1] + 2.0 * ys[i] + ys[i + 1]) * 0.25;
            }
        }
        out
    }
}

/// Catmull-Rom value of `arr` at the fractional index `k`.
pub fn cubic_at(arr: &[f32], k: f32) -> f32 {
    let n = arr.len();
    if n == 0 {
        return 0.0;
    }
    let k = k.clamp(0.0, (n - 1) as f32);
    let i = k.floor() as isize;
    let t = k - i as f32;
    let at = |j: isize| arr[j.clamp(0, n as isize - 1) as usize];
    let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    0.5 * (2.0 * p1 + (-p0 + p2) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t)
}

/// A polyline made smooth: Catmull-Rom between the points (x must rise), `sub` steps each.
pub fn smooth_line(pts: &[Pos2], sub: usize) -> Vec<Pos2> {
    let n = pts.len();
    if n < 3 || sub < 2 {
        return pts.to_vec();
    }
    let at = |j: isize| pts[j.clamp(0, n as isize - 1) as usize];
    let mut out = Vec::with_capacity(n * sub);
    for i in 0..n as isize - 1 {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        for k in 0..sub {
            let t = k as f32 / sub as f32;
            let cr = |a: f32, b: f32, c: f32, d: f32| 0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t + (-a + 3.0 * b - 3.0 * c + d) * t * t * t);
            // x linear (keeps it monotonic), y curved
            out.push(Pos2::new(p1.x + (p2.x - p1.x) * t, cr(p0.y, p1.y, p2.y, p3.y)));
        }
    }
    out.push(pts[n - 1]);
    out
}

/// Filled spectrum with an outline, clipped to `clip`.
pub fn draw_spectrum(painter: &Painter, clip: Rect, pts: &[Pos2], fill: Color32, line: Color32) {
    if pts.len() < 2 {
        return;
    }
    let p = painter.with_clip_rect(clip);
    let pts: Vec<Pos2> = smooth_line(pts, 3).iter().map(|q| Pos2::new(q.x, q.y.min(clip.bottom() + 2.0))).collect();
    // one triangle strip with shared vertices (no seams between columns)
    let mut mesh = nih_plug_egui::egui::epaint::Mesh::default();
    for (i, q) in pts.iter().enumerate() {
        mesh.colored_vertex(*q, fill);
        mesh.colored_vertex(Pos2::new(q.x, clip.bottom()), fill);
        if i > 0 {
            let k = (2 * i) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    p.add(Shape::mesh(mesh));
    p.add(Shape::line(pts, PathStroke::new(1.0, line)));
}

/// Scrolling waveform: keeps min / max per column of the newest audio, newest on the right.
pub struct Wave {
    cols: Vec<(f32, f32)>,
    buf: Vec<f32>,
}

impl Wave {
    pub fn new(columns: usize) -> Self {
        Self { cols: vec![(0.0, 0.0); columns], buf: Vec::new() }
    }

    /// Re-read the last `seconds` of audio from `tap` (it must hold at least that much).
    pub fn update(&mut self, tap: &Tap, fs: f32, seconds: f32) {
        let n = ((fs * seconds) as usize).max(self.cols.len());
        self.buf.resize(n, 0.0);
        tap.snapshot(&mut self.buf);
        let per = n as f32 / self.cols.len() as f32;
        for (c, col) in self.cols.iter_mut().enumerate() {
            let a = (c as f32 * per) as usize;
            let b = (((c + 1) as f32 * per) as usize).clamp(a + 1, n);
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for &s in &self.buf[a..b] {
                lo = lo.min(s);
                hi = hi.max(s);
            }
            *col = (lo, hi);
        }
    }

    /// Draw into `r` (centre line = silence, edges = full scale).
    pub fn draw(&self, painter: &Painter, r: Rect, col: Color32) {
        let w = r.width() / self.cols.len() as f32;
        let cy = r.center().y;
        let h = r.height() * 0.5;
        for (i, (lo, hi)) in self.cols.iter().enumerate() {
            let x = r.left() + (i as f32 + 0.5) * w;
            let (y0, y1) = (cy - hi.clamp(-1.0, 1.0) * h, cy - lo.clamp(-1.0, 1.0) * h);
            painter.line_segment([Pos2::new(x, y0.min(cy - 0.5)), Pos2::new(x, y1.max(cy + 0.5))], Stroke::new(w.max(1.0), col));
        }
    }

    /// Highest absolute sample over the whole view.
    pub fn peak(&self) -> f32 {
        self.cols.iter().map(|(a, b)| a.abs().max(b.abs())).fold(0.0, f32::max)
    }
}
