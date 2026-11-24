//! NyashkEQ 2 GUI, laid out the way Pro-Q users expect it: a big display (double-click adds a
//! band, drag, wheel = Q, Alt + wheel = dynamic range, right-click menu, rubber-band selection,
//! spectrum grab, EQ sketch, piano display), a floating band panel under the display (shape and
//! slope on the left, FREQ / GAIN / Q knobs with the dynamic range ring around GAIN, placement /
//! audition on the right, the dynamics section behind "»"), and the bottom bar (processing mode
//! with resolution, character, analyzer settings, output section).
use nih_plug::prelude::*;
use nih_plug_egui::{
    create_egui_editor,
    egui::{self, epaint::PathStroke, Align2, Color32, CornerRadius, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2},
};
use nyashk_common::analyzer::{cubic_at, smooth_line};
use nyashk_common::fft::Fft;
use nyashk_common::ui::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::conv::{Place, NB};
use crate::filters::{analog, Shape, Slope, Spec};
use crate::params::{fmt_hz, AnRange, AnRes, AnSpeed, AnTilt, BandParams, Character, DispRange, EqParams, PMode, Res};
use crate::Shared;

pub const BASE_W: f32 = 1100.0;
pub const BASE_H: f32 = 728.0;
/// top of the band strip and of the bottom bar
const STRIP_Y: f32 = 492.0;
const BAR_Y: f32 = 650.0;
const F_LO: f32 = 10.0;
const F_HI: f32 = 30000.0;
const NPTS: usize = 360;
/// grid for EQ match / sketch fitting: 1/12 octave, 20 Hz .. 20 kHz
const FIT_N: usize = 121;

fn fit_f(j: usize) -> f32 {
    20.0 * 2f32.powf(j as f32 / 12.0)
}

fn band_col(i: usize) -> Color32 {
    let h = (0.02 + i as f32 * 0.161) % 1.0;
    Color32::from(egui::ecolor::Hsva::new(h, 0.55, 0.92, 1.0))
}

const NOTES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn midi_of(f: f32) -> f32 {
    69.0 + 12.0 * (f / 440.0).log2()
}
fn note_name(f: f32) -> String {
    let m = midi_of(f).round() as i32;
    let cents = ((midi_of(f) - m as f32) * 100.0).round() as i32;
    let base = format!("{}{}", NOTES[m.rem_euclid(12) as usize], m.div_euclid(12) - 1);
    if cents == 0 {
        base
    } else {
        format!("{base} {cents:+}c")
    }
}
fn snap_note(f: f32) -> f32 {
    440.0 * 2f32.powf((midi_of(f).round() - 69.0) / 12.0)
}

#[derive(Clone, Copy)]
struct Disp {
    g: Rect,
    range: f32,
    an_range: f32,
}

impl Disp {
    fn fx(&self, f: f32) -> f32 {
        self.g.left() + self.g.width() * (f.max(1.0) / F_LO).ln() / (F_HI / F_LO).ln()
    }
    fn xf(&self, x: f32) -> f32 {
        F_LO * (F_HI / F_LO).powf(((x - self.g.left()) / self.g.width()).clamp(0.0, 1.0))
    }
    fn dy(&self, d: f32) -> f32 {
        self.g.center().y - self.g.height() * 0.44 * (d / self.range)
    }
    fn yd(&self, y: f32) -> f32 {
        (self.g.center().y - y) / (self.g.height() * 0.44) * self.range
    }
    /// analyzer level (dBFS) to y: the top is +6 dBFS, the bottom -range
    fn sy(&self, db: f32) -> f32 {
        self.g.top() + self.g.height() * ((6.0 - db) / (self.an_range + 6.0)).clamp(-0.02, 1.02)
    }
}

/// what the GUI knows about one band this frame
#[derive(Clone, Copy)]
struct BandView {
    used: bool,
    on: bool,
    spec: Spec,
    place: Place,
    range: f32,
    dyn_now: f32,
    dynamic: bool,
    spectral: bool,
}

/// smoothed spectrum with selectable resolution / speed / freeze
struct Ana {
    n: usize,
    fft: Fft,
    frame: Vec<f32>,
    spec: Vec<f32>,
    sm: Vec<f32>,
}

impl Ana {
    fn new(n: usize) -> Self {
        Self { n, fft: Fft::new(n), frame: vec![0.0; n], spec: vec![-140.0; n / 2], sm: vec![-140.0; n / 2] }
    }
    fn update(&mut self, tap: &nyashk_common::tap::Tap, n: usize, fall: f32, freeze: bool) {
        if n != self.n {
            *self = Ana::new(n);
        }
        tap.snapshot(&mut self.frame);
        self.fft.magnitude_db(&self.frame, &mut self.spec);
        for (s, v) in self.sm.iter_mut().zip(self.spec.iter()) {
            if *v > *s {
                *s += (*v - *s) * 0.7;
            } else if !freeze {
                *s += (*v - *s) * fall;
            }
        }
    }
    fn db_at(&self, f: f32, fs: f32) -> f32 {
        cubic_at(&self.sm, (f * self.n as f32 / fs).clamp(1.0, self.n as f32 / 2.0 - 2.0))
    }
    /// polyline over the display; each point takes the loudest bin it covers
    fn points(&self, d: &Disp, fs: f32, tilt: f32, npts: usize) -> Vec<(f32, f32)> {
        let (f_lo, f_hi) = (20.0f32, F_HI.min(fs * 0.5));
        let n = self.n as f32;
        let bin = |f: f32| (f * n / fs).clamp(1.0, n / 2.0 - 1.0);
        let mut out = Vec::with_capacity(npts + 1);
        for i in 0..=npts {
            let f0 = f_lo * (f_hi / f_lo).powf(i as f32 / npts as f32);
            let f1 = f_lo * (f_hi / f_lo).powf((i as f32 + 1.0) / npts as f32);
            let (b0, b1) = (bin(f0), bin(f1));
            let db = if b1 - b0 < 1.0 { cubic_at(&self.sm, 0.5 * (b0 + b1)) } else { (b0 as usize..=(b1 as usize).min(self.sm.len() - 1)).map(|k| self.sm[k]).fold(-200.0f32, f32::max) };
            out.push((f0, db + tilt * (f0 / 1000.0).log2()));
        }
        let _ = d;
        if out.len() > 2 {
            let ys: Vec<f32> = out.iter().map(|q| q.1).collect();
            for i in 1..out.len() - 1 {
                out[i].1 = (ys[i - 1] + 2.0 * ys[i] + ys[i + 1]) * 0.25;
            }
        }
        out
    }
}

/// what the analyzer pop-up is showing
#[derive(Clone, Copy, PartialEq)]
enum Pop {
    None,
    Analyzer,
}

struct Gui {
    pre: Ana,
    post: Ana,
    sc: Ana,
    out_pk: [f32; 2],
    sel: [bool; NB],
    focus: Option<usize>,
    hover: Option<usize>,
    drag: Option<Drag>,
    rubber: Option<Pos2>,
    dyn_open: bool,
    /// mini toolbar of the dot under the mouse: (band, last time the mouse was on it)
    hbar: Option<(usize, f64)>,
    pop: Pop,
    sketch_on: bool,
    sketch: Vec<Pos2>,
    /// EQ match: long-term average of pre and side chain on the fit grid
    learn: bool,
    m_pre: Vec<f64>,
    m_sc: Vec<f64>,
    m_cnt: u32,
    /// per band response cache: (spec it was computed for, dB per display point)
    cache: Vec<(Option<Spec>, Vec<f32>)>,
    undo: Vec<Vec<f32>>,
    redo: Vec<Vec<f32>>,
    ab: [Option<Vec<f32>>; 2],
    ab_cur: usize,
    last_snap_t: f64,
    pending_snap: bool,
}

struct Drag {
    start: Pos2,
    /// (band, start freq, start gain)
    bands: Vec<(usize, f32, f32)>,
}

/// Greedy fit of bell bands to a target curve on the fit grid (`r` is consumed).
fn fit_bells(r: &mut [f32], j0: usize, j1: usize, max_bands: usize) -> Vec<(f32, f32, f32)> {
    let mut out = Vec::new();
    for _ in 0..max_bands {
        let (mut jm, mut vm) = (j0, 0.0f32);
        for j in j0..=j1 {
            if r[j].abs() > vm.abs() {
                jm = j;
                vm = r[j];
            }
        }
        if vm.abs() < 0.6 {
            break;
        }
        let half = vm * 0.5;
        let mut lo = jm;
        while lo > j0 && r[lo - 1].signum() == vm.signum() && r[lo - 1].abs() > half.abs() {
            lo -= 1;
        }
        let mut hi = jm;
        while hi < j1 && r[hi + 1].signum() == vm.signum() && r[hi + 1].abs() > half.abs() {
            hi += 1;
        }
        let bw = ((hi - lo + 1) as f32 / 12.0).clamp(0.12, 4.0);
        let k = 2f32.powf(bw);
        let q = (k.sqrt() / (k - 1.0)).clamp(0.1, 18.0);
        let f = fit_f(jm);
        let g = vm.clamp(-30.0, 30.0);
        let a = analog(&Spec { shape: Shape::Bell, slope: Slope::S12, freq: f as f64, gain_db: g as f64, q: q as f64, gq: false });
        for (j, v) in r.iter_mut().enumerate() {
            *v -= a.db(fit_f(j) as f64) as f32;
        }
        out.push((f, g, q));
    }
    out
}

/// knob for the band gain with the dynamic range as a draggable outer ring (like Pro-Q)
#[allow(clippy::too_many_arguments)]
fn gain_knob(ui: &mut Ui, setter: &ParamSetter, b: &BandParams, cell: Rect, r: f32, col: Color32, dyn_now: f32, scale: f32) {
    let c = Pos2::new(cell.center().x, cell.top() + r + 4.0);
    // ring first: the knob allocated after it wins in the middle
    let ring = ui.allocate_rect(Rect::from_center_size(c, Vec2::splat(2.0 * r + 26.0)), Sense::click_and_drag()).on_hover_text("Outer ring: dynamic range (drag up = expand, down = compress, double-click = static band).");
    if ring.drag_started() {
        setter.begin_set_parameter(&b.dyn_range);
    }
    if ring.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let dv = -ring.drag_delta().y * if fine { 0.03 } else { 0.15 };
        setter.set_parameter(&b.dyn_range, (b.dyn_range.value() + dv).clamp(-30.0, 30.0));
    }
    if ring.drag_stopped() {
        setter.end_set_parameter(&b.dyn_range);
    }
    if ring.double_clicked() {
        set_f(setter, &b.dyn_range, 0.0);
    }
    knob_ac(ui, setter, &b.gain, "GAIN", cell, "Gain, ±30 dB.", true, r, col);
    let painter = ui.painter();
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let rr = r + 6.0;
    let nrm = |db: f32| ((db + 30.0) / 60.0).clamp(0.0, 1.0);
    let arc = |t0: f32, t1: f32| -> Vec<Pos2> {
        let n = 40;
        (0..=n)
            .map(|k| {
                let t = t0 + (t1 - t0) * k as f32 / n as f32;
                let a = a0 + t * sweep;
                c + Vec2::new(a.cos(), a.sin()) * rr
            })
            .collect()
    };
    painter.add(egui::Shape::line(arc(0.0, 1.0), PathStroke::new(1.0_f32, if ring.hovered() { DIM } else { Color32::from_rgb(38, 38, 38) })));
    let range = b.dyn_range.value();
    if range.abs() > 0.01 {
        let g = b.gain.value();
        painter.add(egui::Shape::line(arc(nrm(g), nrm(g + range)), PathStroke::new(3.0_f32, C_DYN)));
        let t = nrm(g + dyn_now / scale.abs().max(0.05));
        let a = a0 + t * sweep;
        painter.circle_filled(c + Vec2::new(a.cos(), a.sin()) * rr, 3.0, WHITE);
    }
}

// ------------------------------------------------------------------ parameter helpers

fn begin(setter: &ParamSetter, b: &BandParams) {
    setter.begin_set_parameter(&b.freq);
    setter.begin_set_parameter(&b.gain);
}
fn end(setter: &ParamSetter, b: &BandParams) {
    setter.end_set_parameter(&b.freq);
    setter.end_set_parameter(&b.gain);
}

fn snapshot(p: &EqParams) -> Vec<f32> {
    p.param_map().iter().map(|(_, ptr, _)| unsafe { ptr.unmodulated_normalized_value() }).collect()
}

fn restore(setter: &ParamSetter, p: &EqParams, snap: &[f32]) {
    for ((_, ptr, _), &v) in p.param_map().iter().zip(snap.iter()) {
        let cur = unsafe { ptr.unmodulated_normalized_value() };
        if (cur - v).abs() > 1e-7 {
            unsafe {
                setter.raw_context.raw_begin_set_parameter(*ptr);
                setter.raw_context.raw_set_parameter_normalized(*ptr, v);
                setter.raw_context.raw_end_set_parameter(*ptr);
            }
        }
    }
}

/// put a band back to its defaults (everything but `used`)
fn reset_band(setter: &ParamSetter, b: &BandParams) {
    reset(setter, &b.on);
    reset(setter, &b.shape);
    reset(setter, &b.slope);
    reset(setter, &b.freq);
    reset(setter, &b.gain);
    reset(setter, &b.q);
    reset(setter, &b.place);
    reset(setter, &b.gq);
    reset(setter, &b.dyn_range);
    reset(setter, &b.dyn_on);
    reset(setter, &b.thr_auto);
    reset(setter, &b.thr);
    reset(setter, &b.attack);
    reset(setter, &b.release);
    reset(setter, &b.sc_free);
    reset(setter, &b.sc_lo);
    reset(setter, &b.sc_hi);
    reset(setter, &b.sc_ext);
    reset(setter, &b.spectral);
    reset(setter, &b.density);
    reset(setter, &b.sp_tilt);
}

/// a new bell at an explicit (free) index
fn put_bell(setter: &ParamSetter, b: &BandParams, f: f32, g: f32, q: f32) {
    reset_band(setter, b);
    set_f(setter, &b.freq, f);
    set_f(setter, &b.gain, (g * 10.0).round() / 10.0);
    set_f(setter, &b.q, q);
    set_b(setter, &b.used, true);
}

fn free_bands(p: &EqParams) -> Vec<usize> {
    (0..NB).filter(|&i| !p.bands[i].used.value()).collect()
}

fn create_band(setter: &ParamSetter, p: &EqParams, f: f32, g: f32, dynamic: bool) -> Option<usize> {
    let i = p.bands.iter().position(|b| !b.used.value())?;
    let b = &p.bands[i];
    reset_band(setter, b);
    let (shape, gain) = if f < 30.0 {
        (Shape::LowCut, 0.0)
    } else if f > 15000.0 {
        (Shape::HighCut, 0.0)
    } else {
        (Shape::Bell, g)
    };
    set_e(setter, &b.shape, shape);
    set_f(setter, &b.freq, f);
    set_f(setter, &b.gain, (gain * 100.0).round() / 100.0);
    if dynamic && shape == Shape::Bell {
        set_f(setter, &b.dyn_range, -6.0);
    }
    set_b(setter, &b.used, true);
    Some(i)
}

fn delete_band(setter: &ParamSetter, b: &BandParams) {
    set_b(setter, &b.used, false);
    reset_band(setter, b);
}

fn view(p: &EqParams, sh: &Shared, i: usize) -> BandView {
    let b = &p.bands[i];
    let k = p.gain_scale.value() as f64 / 100.0;
    let mut spec = Spec { shape: b.shape.value(), slope: b.slope.value().for_shape(b.shape.value()), freq: b.freq.value() as f64, gain_db: b.gain.value() as f64, q: b.q.value() as f64, gq: b.gq.value() };
    spec.gain_db = (spec.gain_db * k).clamp(-30.0, 30.0);
    let range = b.dyn_range.value();
    BandView {
        used: b.used.value(),
        on: b.on.value(),
        spec,
        place: b.place.value(),
        range: range * k as f32,
        dyn_now: Shared::get(&sh.dyn_db[i]),
        dynamic: spec.shape.has_gain() && range.abs() > 0.01,
        spectral: b.spectral.value(),
    }
}

fn resp(spec: &Spec, d: &Disp, out: &mut Vec<f32>) {
    out.clear();
    let a = analog(spec);
    for k in 0..=NPTS {
        let f = d.xf(d.g.left() + d.g.width() * k as f32 / NPTS as f32);
        out.push(a.db(f as f64) as f32);
    }
}

fn shape_icon(painter: &egui::Painter, r: Rect, s: Shape, col: Color32) {
    let (l, t, w, h) = (r.left(), r.top(), r.width(), r.height());
    let m = t + h * 0.5;
    let pts: Vec<Pos2> = (0..=24)
        .map(|k| {
            let u = k as f32 / 24.0;
            let x = l + u * w;
            let v = match s {
                Shape::Bell => (-((u - 0.5) * 6.0).powi(2)).exp(),
                Shape::Notch => -(1.0 - ((u - 0.5) * 9.0).powi(2).min(1.0)) * 1.2,
                Shape::BandPass => (-((u - 0.5) * 5.0).powi(2)).exp() * 1.6 - 0.8,
                Shape::LowShelf => 1.0 - 1.0 / (1.0 + (-(u - 0.5) * 10.0).exp()) * 1.0,
                Shape::HighShelf => 1.0 / (1.0 + (-(u - 0.5) * 10.0).exp()),
                Shape::LowCut => -(1.0 - (u * 2.2).min(1.0)).powi(2) * 1.6,
                Shape::HighCut => -((u - 0.55) * 2.2).max(0.0).powi(2) * 1.6,
                Shape::TiltShelf => (1.0 / (1.0 + (-(u - 0.5) * 9.0).exp())) * 1.6 - 0.8,
                Shape::FlatTilt => (u - 0.5) * 1.6,
                Shape::AllPass => (u * 12.0).sin() * 0.0,
            };
            Pos2::new(x, m - v * h * 0.38)
        })
        .collect();
    painter.add(egui::Shape::line(pts, PathStroke::new(1.6_f32, col)));
    if s == Shape::AllPass {
        painter.circle_stroke(Pos2::new(l + w * 0.5, m), h * 0.25, Stroke::new(1.4_f32, col));
    }
}

/// headphones symbol
fn headphones(painter: &egui::Painter, c: Pos2, col: Color32) {
    let arc: Vec<Pos2> = (0..=16)
        .map(|k| {
            let a = std::f32::consts::PI + std::f32::consts::PI * k as f32 / 16.0;
            c + Vec2::new(a.cos() * 5.5, a.sin() * 5.5 + 1.0)
        })
        .collect();
    painter.add(egui::Shape::line(arc, PathStroke::new(1.4_f32, col)));
    for sx in [-1.0f32, 1.0] {
        let r = Rect::from_center_size(c + Vec2::new(sx * 5.0, 3.5), Vec2::new(3.0, 5.0));
        painter.rect_filled(r, CornerRadius::same(1), col);
    }
}

/// small text button
fn tbtn(ui: &mut Ui, r: Rect, text: &str, on: bool, tip: &str) -> egui::Response {
    let resp = ui.allocate_rect(r, Sense::click()).on_hover_text(tip);
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect_filled(r, CornerRadius::same(5), if on { WHITE } else if hov { PANEL2 } else { Color32::from_rgb(16, 16, 16) });
    p.rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0_f32, if on { WHITE } else if hov { DIM } else { LINE }), StrokeKind::Inside);
    let gw = ui.fonts(|f| f.layout_no_wrap(text.to_string(), mono(10.5), WHITE).size().x);
    let size = if gw > r.width() - 6.0 { (10.5 * (r.width() - 6.0) / gw).max(7.0) } else { 10.5 };
    p.text(r.center(), Align2::CENTER_CENTER, text, mono(size), if on { Color32::BLACK } else { WHITE });
    resp
}

/// drop-down for an enum parameter with a custom list of labels
fn menu_btn<E: Enum + PartialEq + Copy + 'static>(ui: &mut Ui, setter: &ParamSetter, p: &EnumParam<E>, r: Rect, label: &str, items: &[(E, &str)], tip: &str) {
    let resp = tbtn(ui, r, label, false, tip);
    ui.painter().text(Pos2::new(r.right() - 8.0, r.center().y), Align2::CENTER_CENTER, "▾", mono(9.0), DIM);
    let id = ui.make_persistent_id(("menu", p.name()));
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    egui::popup_below_widget(ui, id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(r.width().max(110.0));
        for (v, name) in items {
            if ui.selectable_label(p.value() == *v, egui::RichText::new(*name).font(mono(11.5))).clicked() {
                set_e(setter, p, *v);
            }
        }
    });
}

/// threshold fader: -80..0 dB, live trigger level, auto marker
fn thr_slider(ui: &mut Ui, setter: &ParamSetter, b: &BandParams, r: Rect, lvl: f32, thr_now: f32) {
    let resp = ui.allocate_rect(r, Sense::click_and_drag()).on_hover_text("Threshold. Drag to set it by hand (auto turns off); double-click or A = automatic threshold that follows the signal.");
    let t = |db: f32| r.bottom() - r.height() * ((db + 80.0) / 80.0).clamp(0.0, 1.0);
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(3), Color32::from_rgb(7, 7, 7));
    // live level
    let ly = t(lvl);
    if lvl > -80.0 {
        p.rect_filled(Rect::from_min_max(Pos2::new(r.left() + 3.0, ly), Pos2::new(r.right() - 3.0, r.bottom() - 1.0)), CornerRadius::same(1), Color32::from_rgb(70, 70, 70));
    }
    let auto = b.thr_auto.value();
    let ty = t(if auto { thr_now } else { b.thr.value() });
    p.hline(r.left()..=r.right(), ty, Stroke::new(2.0_f32, C_DYN));
    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0_f32, LINE), StrokeKind::Inside);
    if resp.drag_started() {
        setter.begin_set_parameter(&b.thr);
        if auto {
            set_b(setter, &b.thr_auto, false);
        }
    }
    if resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            let db = ((r.bottom() - pp.y) / r.height() * 80.0 - 80.0).clamp(-80.0, 0.0);
            setter.set_parameter(&b.thr, db);
        }
    }
    if resp.drag_stopped() {
        setter.end_set_parameter(&b.thr);
    }
    if resp.double_clicked() {
        set_b(setter, &b.thr_auto, !auto);
    }
    let txt = if auto { "AUTO".to_string() } else { format!("{:.0}", b.thr.value()) };
    p.text(Pos2::new(r.center().x, r.bottom() + 9.0), Align2::CENTER_CENTER, txt, mono(9.5), WHITE);
}


pub fn create(params: Arc<EqParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    let editor_state = params.editor_state.clone();
    let size_state = editor_state.clone();
    create_egui_editor(
        editor_state,
        Gui {
            pre: Ana::new(8192),
            post: Ana::new(8192),
            sc: Ana::new(8192),
            out_pk: [-90.0; 2],
            sel: [false; NB],
            focus: None,
            hover: None,
            drag: None,
            rubber: None,
            dyn_open: false,
            hbar: None,
            pop: Pop::None,
            sketch_on: false,
            sketch: Vec::new(),
            learn: false,
            m_pre: vec![0.0; FIT_N],
            m_sc: vec![0.0; FIT_N],
            m_cnt: 0,
            cache: (0..NB).map(|_| (None, Vec::with_capacity(NPTS + 1))).collect(),
            undo: Vec::new(),
            redo: Vec::new(),
            ab: [None, None],
            ab_cur: 0,
            last_snap_t: 0.0,
            pending_snap: true,
        },
        |ctx, _| apply_theme(ctx),
        move |ctx, setter, st| {
            ctx.request_repaint_after(Duration::from_millis(30));
            let p = &*params;
            let sh = &*shared;
            let fs = f32::from_bits(sh.fs.load(Ordering::Relaxed));
            let an_n = p.an_res.value().n();
            let fall = p.an_speed.value().fall();
            let freeze = p.an_freeze.value();
            if p.an_pre.value() || st.learn {
                st.pre.update(&sh.tap_pre, an_n, fall, freeze);
            }
            if p.an_post.value() {
                st.post.update(&sh.tap_post, an_n, fall, freeze);
            }
            if p.an_sc.value() || st.learn {
                st.sc.update(&sh.tap_sc, an_n, fall, freeze);
            }
            if st.learn {
                for j in 0..FIT_N {
                    let f = fit_f(j);
                    st.m_pre[j] += 10f64.powf(st.pre.db_at(f, fs) as f64 / 10.0);
                    st.m_sc[j] += 10f64.powf(st.sc.db_at(f, fs) as f64 / 10.0);
                }
                st.m_cnt += 1;
            }
            for c in 0..2 {
                let o = Shared::get(&sh.out_db[c]);
                st.out_pk[c] = if o > st.out_pk[c] { o } else { st.out_pk[c] - 0.8 };
            }
            // undo: a snapshot after every finished gesture
            let now = ctx.input(|i| i.time);
            let released = ctx.input(|i| i.pointer.any_released() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: false, .. })) || i.smooth_scroll_delta.y != 0.0);
            if released {
                st.pending_snap = true;
                st.last_snap_t = now;
            }
            if st.pending_snap && now - st.last_snap_t > 0.35 && !ctx.input(|i| i.pointer.any_down()) {
                st.pending_snap = false;
                let s = snapshot(p);
                if st.undo.last() != Some(&s) {
                    if !st.undo.is_empty() {
                        st.redo.clear();
                    }
                    st.undo.push(s);
                    if st.undo.len() > 200 {
                        st.undo.remove(0);
                    }
                }
            }

            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(BG)).show(ctx, |ui| {
                let full = ui.max_rect();
                let o = full.min;
                let rect = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(Pos2::new(o.x + x, o.y + y), Vec2::new(w, h));
                header(ui, o, BASE_W, "EQ", "DYNAMIC EQUALIZER");

                // ---------------- header buttons
                let byp = p.bypass.value();
                if tbtn(ui, rect(BASE_W - 14.0 - 74.0, 22.0, 74.0, 24.0), "BYPASS", byp, "Bypass the whole plugin (latency stays).").clicked() {
                    set_b(setter, &p.bypass, !byp);
                }
                let hx = BASE_W - 14.0 - 74.0 - 12.0;
                for (k, name) in [(0usize, "A"), (1, "B")] {
                    let r = rect(hx - 172.0 + k as f32 * 30.0, 22.0, 28.0, 24.0);
                    if tbtn(ui, r, name, st.ab_cur == k, "Two settings to compare: click the other one to switch.").clicked() && st.ab_cur != k {
                        st.ab[st.ab_cur] = Some(snapshot(p));
                        let other = st.ab[k].clone().unwrap_or_else(|| snapshot(p));
                        restore(setter, p, &other);
                        st.ab_cur = k;
                    }
                }
                if tbtn(ui, rect(hx - 110.0, 22.0, 50.0, 24.0), if st.ab_cur == 0 { "A→B" } else { "B→A" }, false, "Copy the current setting to the other slot.").clicked() {
                    st.ab[1 - st.ab_cur] = Some(snapshot(p));
                }
                if tbtn(ui, rect(hx - 54.0, 22.0, 26.0, 24.0), "↶", false, "Undo").clicked() && st.undo.len() > 1 {
                    let cur = st.undo.pop().unwrap();
                    st.redo.push(cur);
                    if let Some(prev) = st.undo.last() {
                        restore(setter, p, prev);
                    }
                    st.pending_snap = false;
                }
                if tbtn(ui, rect(hx - 26.0, 22.0, 26.0, 24.0), "↷", false, "Redo").clicked() {
                    if let Some(s) = st.redo.pop() {
                        restore(setter, p, &s);
                        st.undo.push(s);
                    }
                    st.pending_snap = false;
                }

                // ---------------- display
                let gr = rect(14.0, 66.0, BASE_W - 28.0, STRIP_Y - 6.0 - 66.0);
                let d = Disp { g: gr, range: p.disp.value().db(), an_range: p.an_range.value().db() };
                let rng = d.range;
                let painter = ui.painter().clone();
                painter.rect_filled(gr, CornerRadius::same(6), Color32::from_rgb(9, 10, 12));
                let clip = painter.with_clip_rect(gr.shrink(1.0));
                let piano = p.piano.value();
                for f in [20.0f32, 30.0, 40.0, 50.0, 60.0, 80.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 800.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 8000.0, 10000.0, 20000.0] {
                    let major = matches!(f as i32, 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000);
                    clip.vline(d.fx(f), gr.top()..=gr.bottom(), Stroke::new(1.0_f32, Color32::from_white_alpha(if major { 13 } else { 5 })));
                }
                let step = match p.disp.value() {
                    DispRange::D3 => 1.0,
                    DispRange::D6 => 2.0,
                    DispRange::D12 => 3.0,
                    DispRange::D30 => 6.0,
                };
                let mut v = -(rng / step).floor() * step;
                while v <= rng + 0.01 {
                    let y = d.dy(v);
                    clip.hline(gr.left()..=gr.right(), y, Stroke::new(1.0_f32, Color32::from_white_alpha(if v == 0.0 { 28 } else { 8 })));
                    clip.text(Pos2::new(gr.right() - 36.0, y - 1.0), Align2::RIGHT_BOTTOM, if v == 0.0 { "0".into() } else { format!("{:+.0}", v) }, mono(9.0), if v == 0.0 { MID } else { DIM });
                    v += step;
                }
                // analyzer scale (dBFS) on the far right
                let mut a = 0.0f32;
                while a >= -d.an_range + 0.1 {
                    clip.text(Pos2::new(gr.right() - 6.0, d.sy(a)), Align2::RIGHT_CENTER, format!("{:.0}", a), mono(8.5), Color32::from_rgb(70, 78, 90));
                    a -= if d.an_range > 90.0 { 20.0 } else { 10.0 };
                }
                // analyzer
                let tilt = p.an_tilt.value().db();
                let to_pts = |v: &[(f32, f32)]| -> Vec<Pos2> { v.iter().map(|&(f, db)| Pos2::new(d.fx(f), d.sy(db))).collect() };
                let post_pts = if p.an_post.value() { Some(st.post.points(&d, fs, tilt, 320)) } else { None };
                let pre_pts = if p.an_pre.value() { Some(st.pre.points(&d, fs, tilt, 320)) } else { None };
                let sc_pts = if p.an_sc.value() { Some(st.sc.points(&d, fs, tilt, 320)) } else { None };
                if let Some(pp) = &pre_pts {
                    let pts = to_pts(pp);
                    fill_under(&clip, &smooth_line(&pts, 2), gr.bottom(), Color32::from_rgba_unmultiplied(70, 80, 100, 28));
                    clip.add(egui::Shape::line(smooth_line(&pts, 2), PathStroke::new(1.0_f32, Color32::from_rgb(58, 66, 80))));
                }
                if let Some(pp) = &post_pts {
                    let pts = to_pts(pp);
                    clip.add(egui::Shape::line(smooth_line(&pts, 2), PathStroke::new(1.2_f32, Color32::from_rgb(150, 160, 178))));
                }
                if let Some(sp) = &sc_pts {
                    let pts = to_pts(sp);
                    clip.add(egui::Shape::line(smooth_line(&pts, 2), PathStroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 150))));
                    // collisions: where the side chain and this track are both loud and close
                    if p.an_coll.value() {
                        let mine = post_pts.as_ref().or(pre_pts.as_ref());
                        if let Some(m) = mine {
                            for (a, b) in m.iter().zip(sp.iter()) {
                                if a.1 > -d.an_range + 20.0 && b.1 > -d.an_range + 20.0 && (a.1 - b.1).abs() < 8.0 {
                                    let x = d.fx(a.0);
                                    let y = d.sy(a.1.min(b.1));
                                    clip.line_segment([Pos2::new(x, y), Pos2::new(x, gr.bottom())], Stroke::new(3.5_f32, Color32::from_rgba_unmultiplied(220, 50, 50, 40)));
                                }
                            }
                        }
                    }
                }
                // spectral dynamics: the per-frequency gain they apply right now
                if sh.sp_on.load(Ordering::Relaxed) {
                    let spn = sh.sp_n.load(Ordering::Relaxed) as usize;
                    let mut pts = Vec::with_capacity(321);
                    for k in 0..=320 {
                        let f = 20.0 * (F_HI.min(fs * 0.5) / 20.0).powf(k as f32 / 320.0);
                        let bin = ((f * spn as f32 / fs) as usize).min(spn / 2);
                        let g = Shared::get(&sh.sp_show[bin]);
                        pts.push(Pos2::new(d.fx(f), d.dy(g.clamp(-rng * 1.2, rng * 1.2))));
                    }
                    fill_under(&clip, &pts, d.dy(0.0), Color32::from_rgba_unmultiplied(226, 176, 88, 40));
                    clip.add(egui::Shape::line(pts, PathStroke::new(1.2_f32, C_DYN)));
                }
                // piano keys along the bottom
                let ky = gr.bottom() - 30.0;
                if piano {
                    for m in 12..=135 {
                        let f0 = 440.0 * 2f32.powf((m as f32 - 0.5 - 69.0) / 12.0);
                        let f1 = 440.0 * 2f32.powf((m as f32 + 0.5 - 69.0) / 12.0);
                        let (x0, x1) = (d.fx(f0), d.fx(f1));
                        let black = matches!(m % 12, 1 | 3 | 6 | 8 | 10);
                        let r = Rect::from_min_max(Pos2::new(x0 + 0.3, ky), Pos2::new(x1 - 0.3, ky + 12.0));
                        clip.rect_filled(r, CornerRadius::same(1), if black { Color32::from_rgb(24, 24, 26) } else { Color32::from_rgb(120, 122, 128) });
                        if m % 12 == 0 {
                            clip.text(Pos2::new(r.center().x, ky - 6.0), Align2::CENTER_CENTER, format!("C{}", m / 12 - 1), mono(8.0), DIM);
                        }
                    }
                }
                for f in [20.0f32, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
                    let lab = if f >= 1000.0 { format!("{}k", (f / 1000.0) as i32) } else { format!("{}", f as i32) };
                    clip.text(Pos2::new(d.fx(f), gr.bottom() - 9.0), Align2::CENTER_CENTER, lab, mono(9.5), MID);
                }

                // ---------------- bands: data and curves
                let views: Vec<BandView> = (0..NB).map(|i| view(p, sh, i)).collect();
                for i in 0..NB {
                    if !views[i].used {
                        st.sel[i] = false;
                        if st.focus == Some(i) {
                            st.focus = None;
                        }
                        continue;
                    }
                    let spec = views[i].spec;
                    if st.cache[i].0 != Some(spec) {
                        let mut v = std::mem::take(&mut st.cache[i].1);
                        resp(&spec, &d, &mut v);
                        st.cache[i] = (Some(spec), v);
                    }
                }
                let xs: Vec<f32> = (0..=NPTS).map(|k| gr.left() + gr.width() * k as f32 / NPTS as f32).collect();
                let zero = d.dy(0.0);
                let clampc = |v: f32| v.clamp(-rng * 1.25, rng * 1.25);
                // free side-chain range of the focused band
                if let Some(i) = st.focus {
                    let b = &p.bands[i];
                    if views[i].used && views[i].dynamic && b.sc_free.value() {
                        let r = Rect::from_min_max(Pos2::new(d.fx(b.sc_lo.value()), gr.top()), Pos2::new(d.fx(b.sc_hi.value()), gr.bottom()));
                        clip.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(226, 176, 88, 10));
                        clip.vline(r.left(), gr.top()..=gr.bottom(), Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 90)));
                        clip.vline(r.right(), gr.top()..=gr.bottom(), Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 90)));
                    }
                }
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used {
                        continue;
                    }
                    let col = band_col(i);
                    let hot = st.focus == Some(i) || st.hover == Some(i) || st.sel[i];
                    let pts: Vec<Pos2> = xs.iter().zip(st.cache[i].1.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                    let alpha = if !bv.on { 40 } else if hot { 220 } else { 80 };
                    if hot && bv.on {
                        fill_under(&clip, &pts, zero, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 38));
                    }
                    clip.add(egui::Shape::line(pts, PathStroke::new(if hot { 1.6_f32 } else { 1.0 }, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha))));
                    if bv.dynamic && bv.on && hot {
                        let mut s2 = bv.spec;
                        s2.gain_db = (s2.gain_db + bv.range as f64).clamp(-30.0, 30.0);
                        let mut v2 = Vec::with_capacity(NPTS + 1);
                        resp(&s2, &d, &mut v2);
                        let top: Vec<Pos2> = xs.iter().zip(v2.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                        clip.add(egui::Shape::dashed_line(&top, Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 160)), 4.0, 3.0));
                    }
                }
                // total curves (live, dynamics included)
                let mut tot_a = vec![0.0f32; NPTS + 1];
                let mut tot_b = vec![0.0f32; NPTS + 1];
                let mut split = false;
                let mut live = Vec::with_capacity(NPTS + 1);
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used || !bv.on {
                        continue;
                    }
                    let src: &Vec<f32> = if bv.dynamic && !bv.spectral && bv.dyn_now.abs() > 0.02 {
                        let mut s2 = bv.spec;
                        s2.gain_db = (s2.gain_db + bv.dyn_now as f64).clamp(-30.0, 30.0);
                        resp(&s2, &d, &mut live);
                        &live
                    } else {
                        &st.cache[i].1
                    };
                    let (a, b) = match bv.place {
                        Place::Stereo => (true, true),
                        Place::Left | Place::Mid => (true, false),
                        Place::Right | Place::Side => (false, true),
                    };
                    if a != b {
                        split = true;
                    }
                    for k in 0..=NPTS {
                        if a {
                            tot_a[k] += src[k];
                        }
                        if b {
                            tot_b[k] += src[k];
                        }
                    }
                }
                let line_of = |t: &[f32]| -> Vec<Pos2> { xs.iter().zip(t.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect() };
                if split {
                    let has_ms = views.iter().any(|v| v.used && v.on && matches!(v.place, Place::Mid | Place::Side));
                    clip.add(egui::Shape::line(line_of(&tot_b), PathStroke::new(1.8_f32, Color32::from_rgb(120, 200, 230))));
                    clip.text(Pos2::new(gr.left() + 10.0, gr.top() + 14.0), Align2::LEFT_CENTER, if has_ms { "— MID / LEFT" } else { "— LEFT" }, mono(9.5), WHITE);
                    clip.text(Pos2::new(gr.left() + 10.0, gr.top() + 28.0), Align2::LEFT_CENTER, if has_ms { "— SIDE / RIGHT" } else { "— RIGHT" }, mono(9.5), Color32::from_rgb(120, 200, 230));
                }
                let main = line_of(&tot_a);
                if !split {
                    fill_under(&clip, &main, zero, Color32::from_white_alpha(9));
                }
                clip.add(egui::Shape::line(main, PathStroke::new(2.2_f32, if byp { DIM } else { WHITE })));

                // ---------------- band panel geometry (needed now: the display ignores clicks there)
                let panel_rect = st.focus.filter(|&i| views[i].used).map(|i| {
                    let has_gain = views[i].spec.shape.has_gain();
                    let w = if st.dyn_open && has_gain { 968.0 } else { 600.0 };
                    let h = 150.0;
                    // its own strip under the display: never covers the curve
                    Rect::from_min_size(Pos2::new(o.x + BASE_W * 0.5 - w * 0.5, o.y + STRIP_Y + 1.0), Vec2::new(w, h))
                });
                let ana_rect = if st.pop == Pop::Analyzer { Some(rect(300.0, BAR_Y - 214.0, 440.0, 206.0)) } else { None };

                // ---------------- interaction
                let pointer = ui.input(|i| i.pointer.hover_pos());
                let mods = ui.input(|i| i.modifiers);
                let bg = ui.interact(gr, egui::Id::new("eq_bg"), Sense::click_and_drag());
                let node_pos = |i: usize| -> Pos2 {
                    let bv = &views[i];
                    let y = if bv.spec.shape.has_gain() { bv.spec.gain_db as f32 } else { 0.0 };
                    Pos2::new(d.fx(bv.spec.freq as f32).clamp(gr.left() + 6.0, gr.right() - 6.0), d.dy(y.clamp(-rng, rng)).clamp(gr.top() + 6.0, gr.bottom() - 6.0))
                };
                // spectrum grab: the peak of the analyzer under the pointer
                let mut grab: Option<(f32, Pos2)> = None;
                if p.an_grab.value() && !st.sketch_on && st.drag.is_none() {
                    if let (Some(pp), Some(spts)) = (pointer, post_pts.as_ref().or(pre_pts.as_ref())) {
                        let inside = gr.contains(pp) && !panel_rect.is_some_and(|r| r.contains(pp)) && !ana_rect.is_some_and(|r| r.contains(pp));
                        let near_node = (0..NB).any(|i| views[i].used && node_pos(i).distance(pp) < 16.0);
                        if inside && !near_node {
                            for k in 2..spts.len() - 2 {
                                let v = spts[k].1;
                                if v > spts[k - 1].1 && v >= spts[k + 1].1 && v > spts[k - 2].1 + 2.0 && v > spts[k + 2].1 + 2.0 {
                                    let q = Pos2::new(d.fx(spts[k].0), d.sy(v));
                                    if (q.x - pp.x).abs() < 10.0 && (q.y - pp.y).abs() < 18.0 {
                                        grab = Some((spts[k].0, q));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                let mut new_hover = None;
                let mut node_resps: Vec<(usize, egui::Response)> = Vec::new();
                for i in 0..NB {
                    if !views[i].used {
                        continue;
                    }
                    let pos = node_pos(i);
                    let r = ui.interact(Rect::from_center_size(pos, Vec2::splat(20.0)), egui::Id::new(("eqnode", i)), Sense::click_and_drag());
                    if r.hovered() || r.dragged() {
                        new_hover = Some(i);
                    }
                    node_resps.push((i, r));
                }
                let mut range_handle: Option<(usize, egui::Response)> = None;
                if let Some(i) = st.focus {
                    let bv = &views[i];
                    if bv.used && bv.dynamic {
                        let hp = Pos2::new(node_pos(i).x, d.dy(((bv.spec.gain_db as f32) + bv.range).clamp(-rng, rng)));
                        if !panel_rect.is_some_and(|r| r.contains(hp)) {
                            let r = ui.interact(Rect::from_center_size(hp, Vec2::new(26.0, 12.0)), egui::Id::new(("eqrange", i)), Sense::drag());
                            range_handle = Some((i, r));
                        }
                    }
                }
                st.hover = new_hover;
                let snap = |f: f32| if piano { snap_note(f) } else { f };

                for (i, r) in node_resps.iter() {
                    let i = *i;
                    let b = &p.bands[i];
                    if r.clicked() {
                        if mods.alt {
                            set_b(setter, &b.on, !b.on.value());
                        } else if mods.shift || mods.command {
                            st.sel[i] = !st.sel[i];
                            st.focus = Some(i);
                        } else {
                            st.sel = [false; NB];
                            st.sel[i] = true;
                            st.focus = Some(i);
                        }
                    }
                    if r.secondary_clicked() {
                        st.focus = Some(i);
                        if !st.sel[i] {
                            st.sel = [false; NB];
                            st.sel[i] = true;
                        }
                    }
                    if r.double_clicked() {
                        delete_band(setter, b);
                        st.sel[i] = false;
                        st.focus = None;
                        continue;
                    }
                    if r.drag_started() {
                        if !st.sel[i] {
                            if !(mods.shift || mods.command) {
                                st.sel = [false; NB];
                            }
                            st.sel[i] = true;
                        }
                        st.focus = Some(i);
                        let bands: Vec<(usize, f32, f32)> = (0..NB).filter(|&k| st.sel[k] && views[k].used).map(|k| (k, p.bands[k].freq.value(), p.bands[k].gain.value())).collect();
                        for (k, _, _) in &bands {
                            begin(setter, &p.bands[*k]);
                        }
                        st.drag = Some(Drag { start: r.interact_pointer_pos().unwrap_or(node_pos(i)), bands });
                    }
                    if r.dragged() {
                        if let (Some(dr), Some(pp)) = (st.drag.as_ref(), r.interact_pointer_pos()) {
                            let fine = if mods.shift { 0.15 } else { 1.0 };
                            let dx = (pp.x - dr.start.x) * fine;
                            let dyv = (pp.y - dr.start.y) * fine;
                            let ratio = d.xf(d.g.left() + dx + d.g.width() * 0.5) / d.xf(d.g.left() + d.g.width() * 0.5);
                            let dg = -dyv / (d.g.height() * 0.44) * rng;
                            let one = dr.bands.len() == 1;
                            for (k, f0, g0) in &dr.bands {
                                let bk = &p.bands[*k];
                                let nf = (f0 * ratio).clamp(10.0, 30000.0);
                                setter.set_parameter(&bk.freq, if one { snap(nf) } else { nf });
                                if bk.shape.value().has_gain() {
                                    setter.set_parameter(&bk.gain, ((g0 + dg) * 100.0).round() / 100.0);
                                }
                            }
                        }
                    }
                    if r.drag_stopped() {
                        if let Some(dr) = st.drag.take() {
                            for (k, _, _) in &dr.bands {
                                end(setter, &p.bands[*k]);
                            }
                        }
                    }
                    if r.hovered() {
                        let sc = ui.input(|i| i.smooth_scroll_delta.y);
                        if sc != 0.0 {
                            if mods.alt && b.shape.value().has_gain() {
                                set_f(setter, &b.dyn_range, (b.dyn_range.value() + sc / 30.0).clamp(-30.0, 30.0));
                            } else if b.shape.value().has_q() {
                                set_norm(setter, &b.q, b.q.unmodulated_normalized_value() + sc / 1500.0);
                            }
                        }
                    }
                    r.context_menu(|ui| {
                        ui.set_min_width(180.0);
                        let t = |s: &str| egui::RichText::new(s.to_string()).font(mono(11.5));
                        ui.label(egui::RichText::new(format!("BAND {}", i + 1)).font(mono(11.0)).color(band_col(i)));
                        ui.menu_button(t("Shape"), |ui| {
                            for s in Shape::ALL {
                                if ui.selectable_label(b.shape.value() == s, t(s.short())).clicked() {
                                    set_e(setter, &b.shape, s);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button(t("Slope"), |ui| {
                            for s in Slope::ALL {
                                if ui.selectable_label(b.slope.value() == s, t(&format!("{} dB/oct", s.label()))).clicked() {
                                    set_e(setter, &b.slope, s);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button(t("Stereo placement"), |ui| {
                            for pl in [Place::Stereo, Place::Left, Place::Right, Place::Mid, Place::Side] {
                                if ui.selectable_label(b.place.value() == pl, t(&format!("{pl:?}"))).clicked() {
                                    set_e(setter, &b.place, pl);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.separator();
                        if b.shape.value().has_gain() {
                            let dynamic = b.dyn_range.value().abs() > 0.01;
                            if ui.button(t(if dynamic { "Clear dynamics" } else { "Make dynamic" })).clicked() {
                                set_f(setter, &b.dyn_range, if dynamic { 0.0 } else if b.gain.value() > 0.0 { 6.0 } else { -6.0 });
                                ui.close_menu();
                            }
                            if ui.button(t("Invert gain")).clicked() {
                                set_f(setter, &b.gain, -b.gain.value());
                                set_f(setter, &b.dyn_range, -b.dyn_range.value());
                                ui.close_menu();
                            }
                        }
                        if ui.button(t("Reset band")).clicked() {
                            let (f, sh2) = (b.freq.value(), b.shape.value());
                            reset_band(setter, b);
                            set_f(setter, &b.freq, f);
                            set_e(setter, &b.shape, sh2);
                            ui.close_menu();
                        }
                        if ui.button(t("Duplicate")).clicked() {
                            if let Some(&k) = free_bands(p).first() {
                                let nb = &p.bands[k];
                                reset_band(setter, nb);
                                for (src, dst) in [(&b.freq, &nb.freq), (&b.gain, &nb.gain), (&b.q, &nb.q), (&b.dyn_range, &nb.dyn_range)] {
                                    set_f(setter, dst, src.value());
                                }
                                set_f(setter, &nb.freq, (b.freq.value() * 1.12).min(30000.0));
                                set_e(setter, &nb.shape, b.shape.value());
                                set_e(setter, &nb.slope, b.slope.value());
                                set_e(setter, &nb.place, b.place.value());
                                set_b(setter, &nb.used, true);
                            }
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(t(if b.on.value() { "Disable" } else { "Enable" })).clicked() {
                            set_b(setter, &b.on, !b.on.value());
                            ui.close_menu();
                        }
                        if ui.button(t("Delete")).clicked() {
                            delete_band(setter, b);
                            ui.close_menu();
                        }
                    });
                }
                if let Some((i, r)) = range_handle.as_ref() {
                    let b = &p.bands[*i];
                    if r.drag_started() {
                        setter.begin_set_parameter(&b.dyn_range);
                    }
                    if r.dragged() {
                        if let Some(pp) = r.interact_pointer_pos() {
                            let k = (p.gain_scale.value() / 100.0).abs().max(0.05);
                            let target = d.yd(pp.y) / k - b.gain.value();
                            setter.set_parameter(&b.dyn_range, ((target * 10.0).round() / 10.0).clamp(-30.0, 30.0));
                        }
                    }
                    if r.drag_stopped() {
                        setter.end_set_parameter(&b.dyn_range);
                    }
                }
                let in_overlay = |pp: Pos2| panel_rect.is_some_and(|r| r.contains(pp)) || ana_rect.is_some_and(|r| r.contains(pp));
                let bg_pos = bg.interact_pointer_pos().filter(|pp| !in_overlay(*pp));
                if st.sketch_on {
                    // EQ sketch: draw the curve you want, bands are fitted to it on release
                    if bg.drag_started() {
                        st.sketch.clear();
                    }
                    if bg.dragged() {
                        if let Some(pp) = bg_pos {
                            if st.sketch.last().is_none_or(|q| (q.x - pp.x).abs() >= 2.0) {
                                st.sketch.push(pp);
                            }
                        }
                    }
                    if st.sketch.len() > 1 {
                        clip.add(egui::Shape::line(st.sketch.clone(), PathStroke::new(2.0_f32, Color32::from_rgb(120, 200, 255))));
                    }
                    if bg.drag_stopped() && st.sketch.len() > 2 {
                        let mut pts = std::mem::take(&mut st.sketch);
                        pts.sort_by(|a, b| a.x.total_cmp(&b.x));
                        let (x0, x1) = (pts[0].x, pts[pts.len() - 1].x);
                        let mut r = vec![0.0f32; FIT_N];
                        let (mut j0, mut j1) = (FIT_N, 0);
                        for j in 0..FIT_N {
                            let x = d.fx(fit_f(j));
                            if x < x0 || x > x1 {
                                continue;
                            }
                            j0 = j0.min(j);
                            j1 = j1.max(j);
                            let k = pts.partition_point(|q| q.x < x).clamp(1, pts.len() - 1);
                            let (a, b) = (pts[k - 1], pts[k]);
                            let t = if b.x > a.x { (x - a.x) / (b.x - a.x) } else { 0.0 };
                            let want = d.yd(a.y + (b.y - a.y) * t);
                            // what the curve already does there
                            let cur: f32 = (0..NB).filter(|&i| views[i].used && views[i].on).map(|i| analog(&views[i].spec).db(fit_f(j) as f64) as f32).sum();
                            r[j] = want - cur;
                        }
                        if j1 > j0 {
                            let free = free_bands(p);
                            let fitted = fit_bells(&mut r, j0, j1, free.len().min(10));
                            for ((f, g, q), &k) in fitted.iter().zip(free.iter()) {
                                put_bell(setter, &p.bands[k], *f, *g, *q);
                            }
                        }
                    }
                } else {
                    // background: double-click adds a band, drag selects (or grabs a spectrum
                    // peak), click deselects
                    if bg.double_clicked() {
                        if let Some(pp) = bg_pos {
                            if let Some(i) = create_band(setter, p, snap(d.xf(pp.x)), d.yd(pp.y).clamp(-30.0, 30.0), mods.alt) {
                                st.sel = [false; NB];
                                st.sel[i] = true;
                                st.focus = Some(i);
                            }
                        }
                    } else if bg.clicked() && bg_pos.is_some() {
                        st.sel = [false; NB];
                        st.focus = None;
                        st.pop = Pop::None;
                    }
                    if bg.drag_started() && bg_pos.is_some() {
                        if let Some((gf, _)) = grab {
                            if let Some(i) = create_band(setter, p, snap(gf), 0.0, false) {
                                setter.begin_set_parameter(&p.bands[i].gain);
                                st.sel = [false; NB];
                                st.sel[i] = true;
                                st.focus = Some(i);
                                st.drag = Some(Drag { start: bg_pos.unwrap(), bands: vec![(i, gf, 0.0)] });
                            }
                        } else {
                            st.rubber = bg_pos;
                        }
                    }
                    if bg.dragged() {
                        if let (Some(dr), Some(pp)) = (st.drag.as_ref(), pointer) {
                            // dragging a grabbed peak: vertical = gain
                            if let Some(&(k, _, g0)) = dr.bands.first() {
                                let dg = -(pp.y - dr.start.y) / (d.g.height() * 0.44) * rng;
                                setter.set_parameter(&p.bands[k].gain, ((g0 + dg) * 10.0).round() / 10.0);
                            }
                        }
                    }
                    if let (Some(a), true) = (st.rubber, bg.dragged()) {
                        if let Some(b) = pointer {
                            let rr = Rect::from_two_pos(a, b);
                            clip.rect_filled(rr, CornerRadius::same(2), Color32::from_white_alpha(10));
                            clip.rect_stroke(rr, CornerRadius::same(2), Stroke::new(1.0_f32, DIM), StrokeKind::Inside);
                            for i in 0..NB {
                                if views[i].used {
                                    st.sel[i] = rr.contains(node_pos(i)) || (mods.shift && st.sel[i]);
                                }
                            }
                        }
                    }
                    if bg.drag_stopped() {
                        if let Some(dr) = st.drag.take() {
                            for (k, _, _) in &dr.bands {
                                setter.end_set_parameter(&p.bands[*k].gain);
                            }
                        }
                        if st.rubber.take().is_some() {
                            st.focus = (0..NB).find(|&i| st.sel[i]);
                        }
                    }
                }
                let typing = ui.ctx().wants_keyboard_input();
                if !typing && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
                    for i in 0..NB {
                        if st.sel[i] {
                            delete_band(setter, &p.bands[i]);
                            st.sel[i] = false;
                        }
                    }
                    st.focus = None;
                }
                if !views.iter().any(|v| v.used) && !st.sketch_on {
                    clip.text(gr.center() - Vec2::new(0.0, 40.0), Align2::CENTER_CENTER, "double-click to add a band   ·   Alt + double-click: dynamic band", mono(11.0), DIM);
                    clip.text(gr.center() - Vec2::new(0.0, 22.0), Align2::CENTER_CENTER, "drag a peak of the spectrum to grab it", mono(10.0), DIM);
                }
                if st.sketch_on {
                    clip.text(Pos2::new(gr.center().x, gr.top() + 16.0), Align2::CENTER_CENTER, "EQ SKETCH: draw the curve you want, bands are added to follow it", mono(10.5), Color32::from_rgb(120, 200, 255));
                }
                if let Some((gf, q)) = grab {
                    painter.circle_stroke(q, 7.0, Stroke::new(1.5_f32, WHITE));
                    let lab = if piano { note_name(gf) } else { format!("{} Hz", fmt_hz(gf)) };
                    painter.text(q - Vec2::new(0.0, 14.0), Align2::CENTER_BOTTOM, lab, mono(10.0), WHITE);
                }

                // ---------------- nodes
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used {
                        continue;
                    }
                    let pos = node_pos(i);
                    let col = band_col(i);
                    let hot = st.hover == Some(i);
                    let selected = st.sel[i];
                    let rad = if hot || selected { 8.0 } else { 6.5 };
                    if bv.dynamic && bv.on && !bv.spectral {
                        let ny = d.dy((bv.spec.gain_db as f32 + bv.dyn_now).clamp(-rng, rng));
                        painter.line_segment([pos, Pos2::new(pos.x, ny)], Stroke::new(3.0_f32, C_DYN));
                        painter.circle_filled(Pos2::new(pos.x, ny), 2.5, C_DYN);
                    }
                    painter.circle_filled(pos, rad, if bv.on { col } else { Color32::from_rgb(20, 20, 20) });
                    painter.circle_stroke(pos, rad, Stroke::new(if selected { 2.0_f32 } else { 1.2 }, if selected { WHITE } else { col }));
                    if bv.dynamic {
                        painter.circle_stroke(pos, rad + 3.0, Stroke::new(1.0_f32, C_DYN));
                    }
                    let tag = format!("{}", i + 1);
                    painter.text(pos, Align2::CENTER_CENTER, &tag, mono(if tag.len() > 1 { 8.0 } else { 9.5 }), if bv.on { Color32::BLACK } else { col });
                    if bv.place != Place::Stereo {
                        painter.text(pos + Vec2::new(rad + 2.0, -rad - 1.0), Align2::LEFT_BOTTOM, bv.place.short(), mono(9.0), col);
                    }
                    if hot {
                        let fl = if piano { format!("{} ({})", note_name(bv.spec.freq as f32), fmt_hz(bv.spec.freq as f32)) } else { format!("{} Hz", fmt_hz(bv.spec.freq as f32)) };
                        let txt = if bv.spec.shape.has_gain() {
                            format!("{}  {}  {:+.1} dB  Q {:.2}", bv.spec.shape.short(), fl, bv.spec.gain_db, bv.spec.q)
                        } else {
                            format!("{}  {}  Q {:.2}  {}", bv.spec.shape.short(), fl, bv.spec.q, bv.spec.slope.label())
                        };
                        let gal = painter.layout_no_wrap(txt, mono(10.0), WHITE);
                        let sz = gal.size();
                        let x = (pos.x - sz.x * 0.5).clamp(gr.left() + 4.0, gr.right() - 4.0 - sz.x);
                        // under the dot (the button bar sits above it); above when there is no room
                        let y = if pos.y + 16.0 + sz.y < gr.bottom() - 20.0 { pos.y + 14.0 } else { pos.y - 46.0 - sz.y };
                        painter.rect_filled(Rect::from_min_size(Pos2::new(x, y), sz).expand2(Vec2::new(4.0, 2.0)), 3.0, Color32::from_black_alpha(210));
                        painter.galley(Pos2::new(x, y), gal, WHITE);
                    }
                }
                if let Some((i, _)) = range_handle.as_ref() {
                    let bv = &views[*i];
                    let y = d.dy(((bv.spec.gain_db as f32) + bv.range).clamp(-rng, rng));
                    let rr = Rect::from_center_size(Pos2::new(node_pos(*i).x, y), Vec2::new(18.0, 6.0));
                    painter.rect_filled(rr, CornerRadius::same(2), C_DYN);
                    painter.rect_stroke(rr, CornerRadius::same(2), Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Inside);
                }

                // mini toolbar over the hovered dot (like Pro-Q): power, solo, shape, dynamics, delete
                if let Some(i) = st.hover.filter(|_| st.drag.is_none()) {
                    st.hbar = Some((i, now));
                }
                if let Some((i, t)) = st.hbar {
                    if !views[i].used || st.drag.is_some() {
                        st.hbar = None;
                    } else {
                        let np = node_pos(i);
                        let (bw, bh, gap) = (22.0, 20.0, 3.0);
                        let n_btn = 5.0;
                        let w = n_btn * bw + (n_btn - 1.0) * gap + 8.0;
                        let above = np.y - 24.0 - bh - 8.0 > gr.top();
                        let y = if above { np.y - 22.0 - bh - 8.0 } else { np.y + 20.0 };
                        let x = (np.x - w * 0.5).clamp(gr.left() + 4.0, gr.right() - 4.0 - w);
                        let tb = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, bh + 8.0));
                        // the gap between the dot and the bar counts as "on it"
                        let zone = tb.union(Rect::from_center_size(np, Vec2::splat(24.0)));
                        if pointer.is_some_and(|pp| zone.contains(pp)) {
                            st.hbar = Some((i, now));
                        }
                        if now - t > 0.35 && !pointer.is_some_and(|pp| zone.contains(pp)) {
                            st.hbar = None;
                        } else {
                            let b = &p.bands[i];
                            let bv = views[i];
                            let col = band_col(i);
                            let _ = ui.interact(tb, egui::Id::new("hbar_bg"), Sense::click());
                            painter.rect_filled(tb, CornerRadius::same(6), Color32::from_rgba_unmultiplied(14, 15, 18, 240));
                            painter.rect_stroke(tb, CornerRadius::same(6), Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 140)), StrokeKind::Inside);
                            let br = |k: f32| Rect::from_min_size(Pos2::new(tb.left() + 4.0 + k * (bw + gap), tb.top() + 4.0), Vec2::new(bw, bh));
                            // power
                            let on = b.on.value();
                            let r0 = br(0.0);
                            if tbtn(ui, r0, "", on, "Band on / off").clicked() {
                                set_b(setter, &b.on, !on);
                            }
                            let pc = if on { Color32::BLACK } else { WHITE };
                            let c0 = r0.center() + Vec2::new(0.0, 1.0);
                            let arc: Vec<Pos2> = (0..=20)
                                .map(|k| {
                                    let a = -std::f32::consts::FRAC_PI_2 + 0.6 + (2.0 * std::f32::consts::PI - 1.2) * k as f32 / 20.0;
                                    c0 + Vec2::new(a.cos(), a.sin()) * 5.0
                                })
                                .collect();
                            painter.add(egui::Shape::line(arc, PathStroke::new(1.4_f32, pc)));
                            painter.line_segment([c0 - Vec2::new(0.0, 7.0), c0 - Vec2::new(0.0, 2.0)], Stroke::new(1.4_f32, pc));
                            // shape (pop-up list)
                            let r1 = br(2.0);
                            let sresp = tbtn(ui, r1, "", false, "Shape");
                            shape_icon(&painter, r1.shrink2(Vec2::new(3.0, 4.0)), bv.spec.shape, col);
                            let sid = ui.make_persistent_id(("hbar_shape", i));
                            if sresp.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(sid));
                            }
                            egui::popup_below_widget(ui, sid, &sresp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                                ui.set_min_width(130.0);
                                for sh2 in Shape::ALL {
                                    if ui.selectable_label(bv.spec.shape == sh2, egui::RichText::new(sh2.short()).font(mono(11.5))).clicked() {
                                        set_e(setter, &b.shape, sh2);
                                    }
                                }
                            });
                            if ui.memory(|m| m.is_popup_open(sid)) {
                                st.hbar = Some((i, now));
                            }
                            // dynamics
                            let r2 = br(3.0);
                            if bv.spec.shape.has_gain() {
                                if tbtn(ui, r2, "D", bv.dynamic, "Make the band dynamic / static").clicked() {
                                    set_f(setter, &b.dyn_range, if bv.dynamic { 0.0 } else if b.gain.value() > 0.0 { 6.0 } else { -6.0 });
                                }
                            } else {
                                painter.text(r2.center(), Align2::CENTER_CENTER, "·", mono(10.0), DIM);
                            }
                            // solo (hold)
                            let r3 = br(1.0);
                            let solo_on = sh.solo.load(Ordering::Relaxed) == i as i32;
                            let solo = tbtn(ui, r3, "", solo_on, "Solo: hold to hear only this band's range");
                            headphones(&painter, r3.center(), if solo_on { Color32::BLACK } else { WHITE });
                            let cur = sh.solo.load(Ordering::Relaxed);
                            if solo.is_pointer_button_down_on() {
                                sh.solo.store(i as i32, Ordering::Relaxed);
                            } else if cur == i as i32 && !ui.input(|inp| inp.pointer.primary_down()) {
                                sh.solo.store(-1, Ordering::Relaxed);
                            }
                            // delete
                            if tbtn(ui, br(4.0), "x", false, "Delete the band").clicked() {
                                delete_band(setter, b);
                                st.sel[i] = false;
                                if st.focus == Some(i) {
                                    st.focus = None;
                                }
                                st.hbar = None;
                            }
                        }
                    }
                }

                // display range (top right)
                let drr = Rect::from_min_size(Pos2::new(gr.right() - 128.0, gr.top() + 8.0), Vec2::new(60.0, 20.0));
                if tbtn(ui, drr, &format!("±{:.0} dB", rng), false, "Display range: click to cycle 3 / 6 / 12 / 30 dB.").clicked() {
                    let next = match p.disp.value() {
                        DispRange::D3 => DispRange::D6,
                        DispRange::D6 => DispRange::D12,
                        DispRange::D12 => DispRange::D30,
                        DispRange::D30 => DispRange::D3,
                    };
                    set_e(setter, &p.disp, next);
                }

                // ---------------- band strip
                let strip = rect(14.0, STRIP_Y, BASE_W - 28.0, 152.0);
                panel(ui, strip);
                if panel_rect.is_none() {
                    painter.text(strip.center() - Vec2::new(0.0, 12.0), Align2::CENTER_CENTER, "click a dot to edit its band   ·   double-click the display to add one   ·   Alt + double-click: dynamic band", mono(10.5), DIM);
                    painter.text(strip.center() + Vec2::new(0.0, 10.0), Align2::CENTER_CENTER, "drag: frequency / gain   ·   wheel: Q   ·   Alt + wheel: dynamic range   ·   right-click: menu   ·   double-click a dot: delete", mono(9.5), DIM);
                }
                // ---------------- band panel
                if let (Some(i), Some(pr)) = (st.focus, panel_rect) {
                    let b = &p.bands[i];
                    let bv = views[i];
                    let col = band_col(i);
                    let has_gain = bv.spec.shape.has_gain();
                    let _ = ui.interact(pr, egui::Id::new("band_panel_bg"), Sense::click_and_drag());
                    painter.rect_filled(pr, CornerRadius::same(10), Color32::from_rgb(16, 17, 20));
                    painter.rect_stroke(pr, CornerRadius::same(10), Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 110)), StrokeKind::Inside);
                    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + y), Vec2::new(w, h));
                    // top corners: band power and delete
                    let on = b.on.value();
                    let pw = at(10.0, 10.0, 24.0, 22.0);
                    if tbtn(ui, pw, "", on, "Band on / off (Alt-click the dot does the same).").clicked() {
                        set_b(setter, &b.on, !on);
                    }
                    // power symbol
                    let pc = if on { Color32::BLACK } else { WHITE };
                    let c0 = pw.center() + Vec2::new(0.0, 1.0);
                    let arc: Vec<Pos2> = (0..=24).map(|k| {
                        let a = -std::f32::consts::FRAC_PI_2 + 0.6 + (2.0 * std::f32::consts::PI - 1.2) * k as f32 / 24.0;
                        c0 + Vec2::new(a.cos(), a.sin()) * 6.0
                    }).collect();
                    painter.add(egui::Shape::line(arc, PathStroke::new(1.5_f32, pc)));
                    painter.line_segment([c0 - Vec2::new(0.0, 8.0), c0 - Vec2::new(0.0, 2.0)], Stroke::new(1.5_f32, pc));
                    if tbtn(ui, at(566.0, 10.0, 24.0, 22.0), "x", false, "Delete the band (or double-click its dot).").clicked() {
                        delete_band(setter, b);
                        st.focus = None;
                        st.sel[i] = false;
                    }
                    // left: shape and slope
                    let shape_items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.short())).collect();
                    let sr = at(44.0, 30.0, 136.0, 34.0);
                    menu_btn(ui, setter, &b.shape, sr, "", &shape_items, "Filter shape");
                    shape_icon(&painter, Rect::from_min_size(sr.min + Vec2::new(8.0, 7.0), Vec2::new(30.0, 20.0)), bv.spec.shape, col);
                    painter.text(Pos2::new(sr.left() + 44.0, sr.center().y), Align2::LEFT_CENTER, bv.spec.shape.short(), mono(10.5), WHITE);
                    if bv.spec.shape.has_slope() {
                        let slope_items: Vec<(Slope, String)> = Slope::ALL.iter().filter(|s| s.for_shape(bv.spec.shape) == **s).map(|s| (*s, if *s == Slope::Brick { "Brickwall".to_string() } else { format!("{} dB/oct", s.label()) })).collect();
                        let si: Vec<(Slope, &str)> = slope_items.iter().map(|(s, n)| (*s, n.as_str())).collect();
                        let lab = if bv.spec.slope == Slope::Brick { "BRICKWALL".to_string() } else { format!("{} dB/oct", bv.spec.slope.label()) };
                        menu_btn(ui, setter, &b.slope, at(44.0, 72.0, 136.0, 26.0), &lab, &si, "Slope (steepness)");
                    }
                    // knobs
                    let cell = |x: f32, w: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + 14.0), Vec2::new(w, 110.0));
                    knob_ac(ui, setter, &b.freq, "FREQ", cell(188.0, 86.0), "Frequency, 10 Hz to 30 kHz. With the piano display on, dragging the dot snaps to notes.", false, 24.0, col);
                    if has_gain {
                        gain_knob(ui, setter, b, cell(278.0, 96.0), 30.0, col, bv.dyn_now, p.gain_scale.value() / 100.0);
                    } else {
                        painter.text(cell(278.0, 96.0).center() - Vec2::new(0.0, 14.0), Align2::CENTER_CENTER, "no gain", mono(10.0), DIM);
                    }
                    if bv.spec.shape.has_q() {
                        knob_ac(ui, setter, &b.q, "Q", cell(378.0, 86.0), "Bandwidth / resonance. Mouse wheel over the dot does the same.", false, 24.0, col);
                    }
                    if piano {
                        painter.text(Pos2::new(pr.left() + 231.0, pr.top() + 124.0), Align2::CENTER_CENTER, note_name(bv.spec.freq as f32), mono(9.5), col);
                    }
                    // right: placement, gain-Q, audition, dynamics
                    let pl_items = [(Place::Stereo, "Stereo"), (Place::Left, "Left"), (Place::Right, "Right"), (Place::Mid, "Mid"), (Place::Side, "Side")];
                    menu_btn(ui, setter, &b.place, at(472.0, 40.0, 118.0, 26.0), &format!("{:?}", bv.place).to_uppercase(), &pl_items, "Which channels the band works on: both, left, right, mid (centre) or side.");
                    if bv.spec.shape == Shape::Bell {
                        let gq = b.gq.value();
                        if tbtn(ui, at(472.0, 72.0, 56.0, 24.0), "GAIN-Q", gq, "Gain-Q interaction: the bell gets narrower the more gain it has.").clicked() {
                            set_b(setter, &b.gq, !gq);
                        }
                    }
                    let solo_on = sh.solo.load(Ordering::Relaxed) == i as i32;
                    let er = at(534.0, 72.0, 56.0, 24.0);
                    let ear = tbtn(ui, er, "   SOLO", solo_on, "Hold to hear only the part of the spectrum this band works on.");
                    headphones(&painter, Pos2::new(er.left() + 11.0, er.center().y), if solo_on { Color32::BLACK } else { WHITE });
                    let holding = ear.is_pointer_button_down_on();
                    let cur_solo = sh.solo.load(Ordering::Relaxed);
                    let mouse_down = ui.input(|inp| inp.pointer.primary_down());
                    sh.solo.store(if holding { i as i32 } else if cur_solo == i as i32 && !mouse_down { -1 } else { cur_solo }, Ordering::Relaxed);
                    if has_gain && tbtn(ui, at(472.0, 104.0, 118.0, 24.0), if st.dyn_open { "DYNAMICS «" } else { "DYNAMICS »" }, st.dyn_open || bv.dynamic, "Dynamics settings: threshold, attack, release, spectral mode, side chain.").clicked() {
                        st.dyn_open = !st.dyn_open;
                    }
                    // band navigation (bottom centre)
                    let mut used_idx: Vec<usize> = (0..NB).filter(|&k| views[k].used).collect();
                    used_idx.sort_by(|&a, &c| views[a].spec.freq.total_cmp(&views[c].spec.freq));
                    let pos_in = used_idx.iter().position(|&k| k == i).unwrap_or(0);
                    let nav_y = 124.0;
                    if tbtn(ui, at(10.0, nav_y, 22.0, 20.0), "◀", false, "Previous band (by frequency)").clicked() {
                        let k = used_idx[(pos_in + used_idx.len() - 1) % used_idx.len()];
                        st.focus = Some(k);
                        st.sel = [false; NB];
                        st.sel[k] = true;
                    }
                    painter.text(at(32.0, nav_y, 70.0, 20.0).center(), Align2::CENTER_CENTER, format!("BAND {}", i + 1), mono(10.5), col);
                    if tbtn(ui, at(102.0, nav_y, 22.0, 20.0), "▶", false, "Next band (by frequency)").clicked() {
                        let k = used_idx[(pos_in + 1) % used_idx.len()];
                        st.focus = Some(k);
                        st.sel = [false; NB];
                        st.sel[k] = true;
                    }
                    if st.dyn_open && has_gain {
                        let x0 = 610.0;
                        painter.vline(pr.left() + x0 - 8.0, pr.top() + 12.0..=pr.bottom() - 12.0, Stroke::new(1.0_f32, LINE));
                        let lvl = Shared::get(&sh.lvl_db[i]);
                        let thr_now = Shared::get(&sh.thr_db[i]);
                        painter.text(at(x0, 6.0, 30.0, 12.0).center(), Align2::CENTER_CENTER, "THRESH", mono(8.5), DIM);
                        thr_slider(ui, setter, b, at(x0 + 4.0, 20.0, 22.0, 96.0), lvl, thr_now);
                        let auto = b.thr_auto.value();
                        if tbtn(ui, at(x0 + 30.0, 20.0, 20.0, 18.0), "A", auto, "Automatic threshold (follows the band's own average level).").clicked() {
                            set_b(setter, &b.thr_auto, !auto);
                        }
                        let small = |x: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + 18.0), Vec2::new(62.0, 100.0));
                        knob_ac(ui, setter, &b.attack, "ATTACK", small(x0 + 52.0), "Attack: 50 % = automatic (from the band's frequency), lower = faster, higher = slower.", true, 17.0, C_DYN);
                        knob_ac(ui, setter, &b.release, "RELEASE", small(x0 + 112.0), "Release: 50 % = automatic, lower = faster, higher = slower.", true, 17.0, C_DYN);
                        // spectral
                        let spc = b.spectral.value();
                        if tbtn(ui, at(x0 + 180.0, 14.0, 80.0, 22.0), "SPECTRAL", spc, "Spectral dynamics: only the frequencies inside the band that go over the threshold are processed, each on its own (adds latency).").clicked() {
                            set_b(setter, &b.spectral, !spc);
                        }
                        if spc {
                            knob_ac(ui, setter, &b.density, "DENSITY", Rect::from_min_size(Pos2::new(pr.left() + x0 + 188.0, pr.top() + 40.0), Vec2::new(64.0, 84.0)), "Spectral density: low = only peaks that stick out above their neighbours, high = everything over the threshold.", false, 15.0, C_DYN);
                            let tl = b.sp_tilt.value();
                            if tbtn(ui, at(x0 + 180.0, 124.0, 80.0, 18.0), "TILT 3dB", tl, "Tilt the trigger spectrum by +3 dB/oct (highs trigger as easily as lows).").clicked() {
                                set_b(setter, &b.sp_tilt, !tl);
                            }
                        }
                        // trigger
                        let tx = x0 + 272.0;
                        let free = b.sc_free.value();
                        painter.text(at(tx, 4.0, 76.0, 12.0).center(), Align2::CENTER_CENTER, "TRIGGER", mono(8.5), DIM);
                        if tbtn(ui, at(tx, 18.0, 38.0, 22.0), "BAND", !free, "The dynamics listen to the band's own frequency range.").clicked() {
                            set_b(setter, &b.sc_free, false);
                        }
                        if tbtn(ui, at(tx + 40.0, 18.0, 38.0, 22.0), "FREE", free, "Your own side-chain low / high cut (shown in the display).").clicked() {
                            set_b(setter, &b.sc_free, true);
                        }
                        let ext = b.sc_ext.value();
                        let has_sc = sh.has_sc.load(Ordering::Relaxed);
                        if tbtn(ui, at(tx, 46.0, 78.0, 22.0), if has_sc { "EXT SC ●" } else { "EXT SC" }, ext, "Trigger from the external side-chain input (● = signal present).").clicked() {
                            set_b(setter, &b.sc_ext, !ext);
                        }
                        let don = b.dyn_on.value();
                        if tbtn(ui, at(tx, 74.0, 78.0, 22.0), if don { "DYN ON" } else { "DYN OFF" }, don, "Bypass the dynamics only (the range is kept).").clicked() {
                            set_b(setter, &b.dyn_on, !don);
                        }
                        if tbtn(ui, at(tx, 102.0, 78.0, 22.0), "CLEAR", false, "Clear the dynamics: back to a static band.").clicked() {
                            set_f(setter, &b.dyn_range, 0.0);
                        }
                        if free {
                            knob_ac(ui, setter, &b.sc_lo, "SC LOW", Rect::from_min_size(Pos2::new(pr.left() + 900.0 - 18.0, pr.top() + 8.0), Vec2::new(64.0, 70.0)), "Side-chain low cut.", false, 13.0, C_DYN);
                            knob_ac(ui, setter, &b.sc_hi, "SC HIGH", Rect::from_min_size(Pos2::new(pr.left() + 900.0 - 18.0, pr.top() + 76.0), Vec2::new(64.0, 70.0)), "Side-chain high cut.", false, 13.0, C_DYN);
                        }
                    }
                    if sh.dyn_blocked.load(Ordering::Relaxed) && bv.dynamic {
                        painter.text(Pos2::new(pr.center().x, pr.top() - 10.0), Align2::CENTER_CENTER, "dynamics are off: Linear Phase above High resolution", mono(10.0), C_DYN);
                    }
                }

                // ---------------- analyzer pop-up (closes on a click anywhere else)
                if let Some(ar) = ana_rect {
                    let btn = rect(320.0, BAR_Y + 24.0, 130.0, 26.0);
                    if ui.input(|i| i.pointer.any_pressed()) && pointer.is_some_and(|pp| !ar.contains(pp) && !btn.contains(pp)) {
                        st.pop = Pop::None;
                    }
                    let _ = ui.interact(ar, egui::Id::new("ana_pop_bg"), Sense::click_and_drag());
                    painter.rect_filled(ar, CornerRadius::same(8), Color32::from_rgba_unmultiplied(16, 17, 20, 245));
                    painter.rect_stroke(ar, CornerRadius::same(8), Stroke::new(1.0_f32, LINE), StrokeKind::Inside);
                    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(Pos2::new(ar.left() + x, ar.top() + y), Vec2::new(w, h));
                    let lab = |x: f32, y: f32, t: &str| painter.text(Pos2::new(ar.left() + x, ar.top() + y), Align2::LEFT_CENTER, t, mono(9.0), DIM);
                    lab(12.0, 12.0, "SHOW");
                    for (k, (bp, name, tip)) in [(&p.an_pre, "PRE", "Spectrum before the EQ"), (&p.an_post, "POST", "Spectrum after the EQ"), (&p.an_sc, "SC", "External side-chain spectrum")].into_iter().enumerate() {
                        let v = bp.value();
                        if tbtn(ui, at(12.0 + k as f32 * 50.0, 22.0, 46.0, 22.0), name, v, tip).clicked() {
                            set_b(setter, bp, !v);
                        }
                    }
                    let fz = p.an_freeze.value();
                    if tbtn(ui, at(172.0, 22.0, 66.0, 22.0), "FREEZE", fz, "Hold the spectrum (shows the maximum over time).").clicked() {
                        set_b(setter, &p.an_freeze, !fz);
                    }
                    let gb = p.an_grab.value();
                    if tbtn(ui, at(242.0, 22.0, 66.0, 22.0), "GRAB", gb, "Spectrum grab: drag a peak of the spectrum to make a band there.").clicked() {
                        set_b(setter, &p.an_grab, !gb);
                    }
                    let cl = p.an_coll.value();
                    if tbtn(ui, at(312.0, 22.0, 116.0, 22.0), "COLLISIONS", cl, "Shows in red where this track and the side chain are both loud (masking).").clicked() {
                        set_b(setter, &p.an_coll, !cl);
                    }
                    let row = |ui: &mut Ui, y: f32, name: &str, items: &[(&str, bool)]| -> Option<usize> {
                        painter.text(Pos2::new(ar.left() + 12.0, ar.top() + y + 11.0), Align2::LEFT_CENTER, name, mono(9.0), DIM);
                        let mut hit = None;
                        let w = (330.0 - 4.0 * (items.len() as f32 - 1.0)) / items.len() as f32;
                        for (k, (t, on)) in items.iter().enumerate() {
                            if tbtn(ui, Rect::from_min_size(Pos2::new(ar.left() + 98.0 + k as f32 * (w + 4.0), ar.top() + y), Vec2::new(w, 22.0)), t, *on, "").clicked() {
                                hit = Some(k);
                            }
                        }
                        hit
                    };
                    let rv = p.an_range.value();
                    if let Some(k) = row(ui, 54.0, "RANGE", &[("60 dB", rv == AnRange::R60), ("90 dB", rv == AnRange::R90), ("120 dB", rv == AnRange::R120)]) {
                        set_e(setter, &p.an_range, [AnRange::R60, AnRange::R90, AnRange::R120][k]);
                    }
                    let rs = p.an_res.value();
                    if let Some(k) = row(ui, 80.0, "RESOLUTION", &[("LOW", rs == AnRes::Low), ("MEDIUM", rs == AnRes::Medium), ("HIGH", rs == AnRes::High), ("MAX", rs == AnRes::Max)]) {
                        set_e(setter, &p.an_res, [AnRes::Low, AnRes::Medium, AnRes::High, AnRes::Max][k]);
                    }
                    let sp = p.an_speed.value();
                    let sps = [AnSpeed::VSlow, AnSpeed::Slow, AnSpeed::Medium, AnSpeed::Fast, AnSpeed::VFast];
                    if let Some(k) = row(ui, 106.0, "SPEED", &[("V.SLOW", sp == sps[0]), ("SLOW", sp == sps[1]), ("MED", sp == sps[2]), ("FAST", sp == sps[3]), ("V.FAST", sp == sps[4])]) {
                        set_e(setter, &p.an_speed, sps[k]);
                    }
                    let tl = p.an_tilt.value();
                    let tls = [AnTilt::T0, AnTilt::T15, AnTilt::T3, AnTilt::T45, AnTilt::T6];
                    if let Some(k) = row(ui, 132.0, "TILT", &[("0", tl == tls[0]), ("1.5", tl == tls[1]), ("3", tl == tls[2]), ("4.5", tl == tls[3]), ("6", tl == tls[4])]) {
                        set_e(setter, &p.an_tilt, tls[k]);
                    }
                    // EQ match
                    painter.hline(ar.left() + 10.0..=ar.right() - 10.0, ar.top() + 164.0, Stroke::new(1.0_f32, LINE));
                    lab(12.0, 184.0, "EQ MATCH");
                    let secs = st.m_cnt as f32 * 0.03;
                    if tbtn(ui, at(98.0, 174.0, 110.0, 22.0), if st.learn { "LISTENING…" } else { "LEARN" }, st.learn, "Play your track with the reference on the side-chain input; click to start / stop learning both spectra.").clicked() {
                        st.learn = !st.learn;
                    }
                    let ready = st.m_cnt > 30;
                    if tbtn(ui, at(212.0, 174.0, 80.0, 22.0), "MATCH", false, "Add bands that make this track's spectrum follow the reference.").clicked() && ready {
                        st.learn = false;
                        // target = reference - this track, level-matched over 100 Hz .. 10 kHz
                        let mut r: Vec<f32> = (0..FIT_N).map(|j| (10.0 * ((st.m_sc[j] + 1e-12) / (st.m_pre[j] + 1e-12)).log10()) as f32).collect();
                        let (mut s, mut c) = (0.0, 0.0);
                        for (j, v) in r.iter().enumerate() {
                            if fit_f(j) > 100.0 && fit_f(j) < 10000.0 {
                                s += v;
                                c += 1.0;
                            }
                        }
                        let m = s / c;
                        let raw: Vec<f32> = r.iter().map(|v| (v - m).clamp(-12.0, 12.0)).collect();
                        // smooth over 1/3 octave
                        for j in 0..FIT_N {
                            let (lo, hi) = (j.saturating_sub(2), (j + 2).min(FIT_N - 1));
                            r[j] = raw[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
                        }
                        let free = free_bands(p);
                        let fitted = fit_bells(&mut r, 0, FIT_N - 1, free.len().min(8));
                        for ((f, g, q), &k) in fitted.iter().zip(free.iter()) {
                            put_bell(setter, &p.bands[k], *f, *g, *q);
                        }
                    }
                    if tbtn(ui, at(296.0, 174.0, 60.0, 22.0), "RESET", false, "Forget what was learned.").clicked() {
                        st.m_pre.iter_mut().for_each(|v| *v = 0.0);
                        st.m_sc.iter_mut().for_each(|v| *v = 0.0);
                        st.m_cnt = 0;
                    }
                    painter.text(Pos2::new(ar.left() + 364.0, ar.top() + 185.0), Align2::LEFT_CENTER, format!("{:.0} s", secs), mono(9.5), if ready { WHITE } else { DIM });
                }

                // ---------------- bottom bar
                let by = BAR_Y;
                let bar = rect(14.0, by, BASE_W - 28.0, 70.0);
                panel(ui, bar);
                let blab = |x: f32, t: &str| painter.text(Pos2::new(o.x + x, o.y + by + 13.0), Align2::LEFT_CENTER, t, mono(8.5), DIM);
                blab(28.0, "PROCESSING");
                let mode = p.pmode.value();
                let res_name = |r: Res| match r {
                    Res::Low => "Low",
                    Res::Medium => "Medium",
                    Res::High => "High",
                    Res::VeryHigh => "Very High",
                    Res::Max => "Maximum",
                };
                let mode_lab = match mode {
                    PMode::Zero => "ZERO LATENCY".to_string(),
                    PMode::Natural => "NATURAL PHASE".to_string(),
                    PMode::Linear => format!("LINEAR · {}", res_name(p.res.value()).to_uppercase()),
                };
                let mr = rect(28.0, by + 24.0, 176.0, 26.0);
                let mresp = tbtn(ui, mr, &mode_lab, false, "Zero Latency: analog-matched filters, no delay. Natural Phase: analog magnitude and phase, a few ms of latency. Linear Phase: no phase shift at all (pre-ringing), latency set by the resolution.");
                painter.text(Pos2::new(mr.right() - 8.0, mr.center().y), Align2::CENTER_CENTER, "▾", mono(9.0), DIM);
                let pid = ui.make_persistent_id("pmode_pop");
                if mresp.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pid));
                }
                egui::popup_below_widget(ui, pid, &mresp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(200.0);
                    let t = |s: &str| egui::RichText::new(s.to_string()).font(mono(11.5));
                    for (m, name) in [(PMode::Zero, "Zero Latency"), (PMode::Natural, "Natural Phase"), (PMode::Linear, "Linear Phase")] {
                        if ui.selectable_label(mode == m, t(name)).clicked() {
                            set_e(setter, &p.pmode, m);
                        }
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("LINEAR PHASE RESOLUTION").font(mono(9.0)).color(DIM));
                    for r in [Res::Low, Res::Medium, Res::High, Res::VeryHigh, Res::Max] {
                        let lat_ms = (r.n48() as f32 / 2.0 + 512.0) / 48000.0 * 1000.0;
                        let txt = format!("{:<10}{:>6.0} ms", res_name(r), lat_ms);
                        if ui.selectable_label(p.res.value() == r, t(&txt)).clicked() {
                            set_e(setter, &p.res, r);
                            set_e(setter, &p.pmode, PMode::Linear);
                        }
                    }
                });
                let lat = sh.latency.load(Ordering::Relaxed);
                painter.text(Pos2::new(o.x + 28.0, o.y + by + 60.0), Align2::LEFT_CENTER, format!("latency {:.1} ms{}", lat as f32 / fs * 1000.0, if sh.sp_on.load(Ordering::Relaxed) { " (spectral)" } else { "" }), mono(9.0), DIM);

                blab(214.0, "CHARACTER");
                let ch_items = [(Character::Clean, "CLEAN"), (Character::Subtle, "SUBTLE"), (Character::Warm, "WARM")];
                let chl = ch_items.iter().find(|(m, _)| *m == p.character.value()).map(|x| x.1).unwrap_or("");
                menu_btn(ui, setter, &p.character, rect(214.0, by + 24.0, 96.0, 26.0), chl, &ch_items, "Clean: no colour. Subtle / Warm: analog-style saturation (oversampled).");

                blab(320.0, "ANALYZER");
                let an_any = p.an_pre.value() || p.an_post.value() || p.an_sc.value();
                let alab = format!("{}{}", if an_any { "PRE·POST" } else { "OFF" }, if freeze { " ❄" } else { "" });
                let alab = if an_any {
                    let mut v = vec![];
                    if p.an_pre.value() {
                        v.push("PRE");
                    }
                    if p.an_post.value() {
                        v.push("POST");
                    }
                    if p.an_sc.value() {
                        v.push("SC");
                    }
                    format!("{}{}", v.join("+"), if freeze { " ❄" } else { "" })
                } else {
                    alab
                };
                if tbtn(ui, rect(320.0, by + 24.0, 130.0, 26.0), &alab, st.pop == Pop::Analyzer, "Analyzer settings: pre / post / side chain, range, resolution, speed, tilt, freeze, spectrum grab, collisions, EQ match.").clicked() {
                    st.pop = if st.pop == Pop::Analyzer { Pop::None } else { Pop::Analyzer };
                }
                blab(460.0, "DISPLAY");
                if tbtn(ui, rect(460.0, by + 24.0, 64.0, 26.0), "PIANO", piano, "Piano display: notes along the frequency axis, band frequencies snap to notes.").clicked() {
                    set_b(setter, &p.piano, !piano);
                }
                if tbtn(ui, rect(528.0, by + 24.0, 70.0, 26.0), "SKETCH", st.sketch_on, "EQ Sketch: draw a curve in the display and bands are made to follow it.").clicked() {
                    st.sketch_on = !st.sketch_on;
                    st.sketch.clear();
                }

                blab(616.0, "OUTPUT");
                let ag = p.auto_gain.value();
                if tbtn(ui, rect(616.0, by + 24.0, 86.0, 26.0), "AUTO GAIN", ag, "Keeps the overall level about the same whatever the EQ curve does.").clicked() {
                    set_b(setter, &p.auto_gain, !ag);
                }
                let ph = p.phase.value();
                if tbtn(ui, rect(706.0, by + 24.0, 30.0, 26.0), "Ø", ph, "Invert the output polarity.").clicked() {
                    set_b(setter, &p.phase, !ph);
                }
                let kc = |x: f32| Rect::from_min_size(Pos2::new(o.x + x, o.y + by + 1.0), Vec2::new(70.0, 70.0));
                knob_r(ui, setter, &p.gain_scale, "SCALE", kc(744.0), "Gain scale: scales every band's gain and dynamic range (negative turns the curve upside down).", false, 13.0);
                knob_r(ui, setter, &p.out, "OUTPUT", kc(814.0), "Output gain.", true, 13.0);
                knob_r(ui, setter, &p.pan, "PAN", kc(884.0), "Output balance.", true, 13.0);
                for c in 0..2 {
                    vbar(ui, rect(974.0 + c as f32 * 12.0, by + 8.0, 9.0, 54.0), st.out_pk[c], st.out_pk[c], -48.0, 3.0);
                }
                let pk = st.out_pk[0].max(st.out_pk[1]).max(-90.0);
                painter.text(Pos2::new(o.x + 1034.0, o.y + by + 35.0), Align2::CENTER_CENTER, format!("{:.1}", pk), mono(10.0), if pk > 0.0 { Color32::from_rgb(230, 90, 80) } else { DIM });

                resize_grip(ui, ctx, &size_state, full, BASE_W, BASE_H, 760.0);
            });
        },
    )
}
