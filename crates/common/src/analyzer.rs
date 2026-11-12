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
                // interpolate between neighbouring bins
                let k = b0.floor() as usize;
                let t = b0 - k as f32;
                self.smooth[k] * (1.0 - t) + self.smooth[(k + 1).min(self.smooth.len() - 1)] * t
            } else {
                (b0 as usize..=(b1 as usize).min(self.smooth.len() - 1)).map(|k| self.smooth[k]).fold(-200.0f32, f32::max)
            };
            out.push(Pos2::new(fx(f0), dy(db + tilt * (f0 / 1000.0).log2())));
        }
        out
    }
}

/// Filled spectrum with an outline, clipped to `clip`.
pub fn draw_spectrum(painter: &Painter, clip: Rect, pts: &[Pos2], fill: Color32, line: Color32) {
    if pts.len() < 2 {
        return;
    }
    let p = painter.with_clip_rect(clip);
    let pts: Vec<Pos2> = pts.iter().map(|q| Pos2::new(q.x, q.y.min(clip.bottom() + 2.0))).collect();
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
