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
pub const BASE_H: f32 = 660.0;
/// top of the bottom bar
const BAR_Y: f32 = 628.0;
/// width of the output meter column on the right
const METER_W: f32 = 44.0;
const F_LO: f32 = 10.0;
const F_HI: f32 = 30000.0;
/// points of the response curves across the display: about one per pixel even on a large or
/// HiDPI window, so narrow bands (Q 40) and deep notches keep their true shape and depth
const NPTS: usize = 1440;
/// grid for EQ match / sketch fitting: 1/12 octave, 20 Hz .. 20 kHz
const FIT_N: usize = 121;

fn fit_f(j: usize) -> f32 {
    20.0 * 2f32.powf(j as f32 / 12.0)
}

/// band colours: our own palette, evenly spaced hues at one saturation / lightness so no band
/// shouts louder than another
fn band_col(i: usize) -> Color32 {
    let order = [0usize, 5, 2, 7, 4, 9, 1, 6, 3, 8, 10, 11];
    let h = (order[i % 12] as f32 / 12.0 + 0.97) % 1.0;
    let (s, l) = if i < 12 { (0.78f32, 0.64f32) } else { (0.55, 0.74) };
    // HSL -> RGB
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h * 6.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(to(r), to(g), to(b))
}

// ---- design tokens: 4 surface levels, 2 text levels, one brand accent (amber), band colours
const C_BG: Color32 = Color32::from_rgb(13, 14, 18);
const C_SURF: Color32 = Color32::from_rgb(22, 24, 30);
const C_BTN: Color32 = Color32::from_rgb(32, 35, 43);
const C_BTN_H: Color32 = Color32::from_rgb(44, 48, 58);
const C_BORDER: Color32 = Color32::from_rgb(40, 44, 54);
const C_TXT: Color32 = Color32::from_rgb(232, 234, 239);
const C_TXT2: Color32 = Color32::from_rgb(138, 145, 160);

/// undo / redo arrow (dir -1 / 1)
fn icon_arrow(painter: &egui::Painter, c: Pos2, dir: f32, col: Color32) {
    let pts: Vec<Pos2> = (0..=14)
        .map(|k| {
            let a = std::f32::consts::PI * (1.15 - 0.95 * k as f32 / 14.0);
            c + Vec2::new(-dir * a.cos() * 5.0, -a.sin() * 5.0 + 2.0)
        })
        .collect();
    let tip = pts[0];
    painter.add(egui::Shape::line(pts, PathStroke::new(1.4_f32, col)));
    painter.line_segment([tip, tip + Vec2::new(dir * 3.5, -1.0)], Stroke::new(1.4_f32, col));
    painter.line_segment([tip, tip + Vec2::new(dir * 1.0, -3.8)], Stroke::new(1.4_f32, col));
}

/// scissors (split)
fn icon_split(painter: &egui::Painter, c: Pos2, col: Color32) {
    painter.circle_stroke(c + Vec2::new(-3.5, 3.5), 2.2, Stroke::new(1.3_f32, col));
    painter.circle_stroke(c + Vec2::new(3.5, 3.5), 2.2, Stroke::new(1.3_f32, col));
    painter.line_segment([c + Vec2::new(-2.0, 1.6), c + Vec2::new(3.0, -5.0)], Stroke::new(1.3_f32, col));
    painter.line_segment([c + Vec2::new(2.0, 1.6), c + Vec2::new(-3.0, -5.0)], Stroke::new(1.3_f32, col));
}

/// x
fn icon_close(painter: &egui::Painter, c: Pos2, col: Color32) {
    let e = 3.8;
    painter.line_segment([c + Vec2::new(-e, -e), c + Vec2::new(e, e)], Stroke::new(1.4_f32, col));
    painter.line_segment([c + Vec2::new(-e, e), c + Vec2::new(e, -e)], Stroke::new(1.4_f32, col));
}

/// small caption (uppercase, dim)
fn caption(painter: &egui::Painter, pos: Pos2, align: Align2, t: &str) {
    painter.text(pos, align, t, mono(8.5), C_TXT2);
}

/// icon button: same size / radius as the text buttons, the icon drawn by `draw`
fn ibtn(ui: &mut Ui, r: Rect, on: bool, tip: &str, draw: impl FnOnce(&egui::Painter, Pos2, Color32)) -> egui::Response {
    let resp = tbtn(ui, r, "", on, tip);
    let col = if on { Color32::BLACK } else { C_TXT };
    draw(ui.painter(), r.center(), col);
    resp
}

/// theme: the shared Nyashk colours plus the Inter typeface (SIL Open Font License, see
/// assets/Inter-LICENSE.txt) for all text: clean, modern, easy to read at small sizes
fn eq_theme(ctx: &egui::Context) {
    apply_theme(ctx);
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/Inter-Medium.ttf"))));
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(fam).or_default().insert(0, "inter".into());
    }
    ctx.set_fonts(fonts);
    let mut v = ctx.style().visuals.clone();
    v.window_fill = Color32::from_rgb(23, 25, 30);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(40, 43, 51));
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(10);
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(140) };
    v.widgets.hovered.weak_bg_fill = Color32::from_white_alpha(18);
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.selection.bg_fill = Color32::from_white_alpha(30);
    ctx.set_visuals(v);
}

/// area between a curve and a base line, strong at the curve and fading towards the base
fn grad_fill(painter: &egui::Painter, pts: &[Pos2], base: f32, col: Color32, a_curve: u8, a_base: u8) {
    if pts.len() < 2 {
        return;
    }
    let mut m = egui::Mesh::default();
    let ct = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a_curve);
    let cb = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a_base);
    for q in pts {
        m.colored_vertex(*q, ct);
        m.colored_vertex(Pos2::new(q.x, base), cb);
    }
    for k in 0..pts.len() as u32 - 1 {
        let i = 2 * k;
        m.add_triangle(i, i + 1, i + 2);
        m.add_triangle(i + 1, i + 3, i + 2);
    }
    painter.add(egui::Shape::mesh(m));
}

/// vertical gradient rectangle
fn grad(painter: &egui::Painter, r: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.right_bottom(), bottom);
    m.colored_vertex(r.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(m));
}

/// floating panel: flat, solid, rounded, a soft shadow under it
fn glass(painter: &egui::Painter, r: Rect, accent: Option<Color32>) {
    let rr = CornerRadius::same(12);
    for k in 1..=4 {
        let e = k as f32 * 2.0;
        painter.rect_filled(r.translate(Vec2::new(0.0, 4.0)).expand(e), CornerRadius::same(12 + 2 * k as u8), Color32::from_black_alpha(18));
    }
    painter.rect_filled(r, rr, Color32::from_rgba_unmultiplied(22, 24, 30, 236));
    let bc = accent.map(|c| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 70)).unwrap_or(Color32::from_rgb(40, 43, 51));
    painter.rect_stroke(r, rr, Stroke::new(1.0_f32, bc), StrokeKind::Inside);
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
    /// visible frequency range (zoom on the frequency scale)
    lo: f32,
    hi: f32,
}

impl Disp {
    fn fx(&self, f: f32) -> f32 {
        self.g.left() + self.g.width() * (f.max(1.0) / self.lo).ln() / (self.hi / self.lo).ln()
    }
    fn xf(&self, x: f32) -> f32 {
        self.lo * (self.hi / self.lo).powf(((x - self.g.left()) / self.g.width()).clamp(0.0, 1.0))
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
        let (f_lo, f_hi) = (d.lo.max(10.0), d.hi.min(fs * 0.5));
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
    /// opened by hovering the button (closes when the mouse leaves)
    AnalyzerHover,
    /// clicked: stays until clicked again / elsewhere
    Analyzer,
}

/// typing exact values for a band (double-click a dot)
struct Edit {
    band: usize,
    text: [String; 3],
    first: bool,
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
    /// parameter display of the dot under the mouse: (band, last time the mouse was near)
    pbox: Option<(usize, f64)>,
    /// solo drag: (band, start pos, start freq, start level, start Q)
    solo_drag: Option<(usize, Pos2, f32, f32, f32)>,
    edit: Option<Edit>,
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
    /// frequency zoom (visible range) and the range the band caches were made for
    zoom: (f32, f32),
    cache_zoom: (f32, f32),
    /// frequency-scale drag: (start, lo, hi)
    scale_drag: Option<(Pos2, f32, f32)>,
    /// spectrum grab: 0 off, 1 temporary (after hovering the spectrum), 2 permanent (click and hold)
    grab: u8,
    /// since when the mouse rests on the spectrum (-1: not)
    grab_t: f64,
    /// the frozen spectrum while grabbing
    grab_pts: Option<Vec<(f32, f32)>>,
    /// the current press must not also make / deselect a band (it switched grab mode)
    eat_click: bool,
    /// FREEZE held down: frozen until released; when it went down
    freeze_hold: bool,
    freeze_t: Option<f64>,
}

/// two snapshots are the same setting (restoring goes through normalized values: tiny rounding)
fn same_snap(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
}

/// undo one step. A change not yet recorded (made less than 0.35 s ago) is undone first.
fn do_undo(st: &mut Gui, setter: &ParamSetter, p: &EqParams) {
    let cur = snapshot(p);
    if st.undo.last().is_some_and(|l| !same_snap(l, &cur)) {
        st.redo.push(cur);
        let prev = st.undo.last().cloned().unwrap();
        restore(setter, p, &prev);
    } else if st.undo.len() > 1 {
        let cur = st.undo.pop().unwrap();
        st.redo.push(cur);
        if let Some(prev) = st.undo.last().cloned() {
            restore(setter, p, &prev);
        }
    }
    st.pending_snap = false;
}

fn do_redo(st: &mut Gui, setter: &ParamSetter, p: &EqParams) {
    if let Some(sn) = st.redo.pop() {
        restore(setter, p, &sn);
        st.undo.push(sn);
    }
    st.pending_snap = false;
}

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    /// frequency + gain (Ctrl: Q)
    Move,
    /// a band just made by dragging the curve / a spectrum peak: gain only
    Create,
}

struct Drag {
    start: Pos2,
    /// (band, start freq, start gain, start Q)
    bands: Vec<(usize, f32, f32, f32)>,
    kind: DragKind,
    /// Alt: the axis chosen after the first few pixels (0 = not yet, 1 = horizontal, 2 = vertical)
    axis: u8,
}

/// bands copied with right-click > Copy, shared by every NyashkEQ in the same host
static CLIP: std::sync::Mutex<Vec<Vec<(String, f32)>>> = std::sync::Mutex::new(Vec::new());

/// the normalized value of every parameter of band i, keyed by the id without its "_n" suffix
fn copy_band(p: &EqParams, i: usize) -> Vec<(String, f32)> {
    let suf = format!("_{}", i + 1);
    p.param_map()
        .iter()
        .filter_map(|(id, ptr, _)| id.strip_suffix(&suf).map(|k| (k.to_string(), unsafe { ptr.unmodulated_normalized_value() })))
        .collect()
}

fn paste_band(setter: &ParamSetter, p: &EqParams, k: usize, vals: &[(String, f32)]) {
    let suf = format!("_{}", k + 1);
    let map = p.param_map();
    for (key, v) in vals {
        let want = format!("{key}{suf}");
        if let Some((_, ptr, _)) = map.iter().find(|(id, _, _)| *id == want) {
            unsafe {
                setter.raw_context.raw_begin_set_parameter(*ptr);
                setter.raw_context.raw_set_parameter_normalized(*ptr, *v);
                setter.raw_context.raw_end_set_parameter(*ptr);
            }
        }
    }
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

/// Typing a value into a knob (double-click): an edit field over the value, Enter sets it,
/// Escape or clicking elsewhere cancels. True while editing.
fn knob_text_edit<P: Param>(ui: &mut Ui, setter: &ParamSetter, p: &P, id: egui::Id, c: Pos2, r: f32, start: bool) -> bool {
    let tid = id.with("txt");
    let mut text: Option<String> = ui.data(|d| d.get_temp(tid));
    if start {
        text = Some(p.normalized_value_to_string(p.unmodulated_normalized_value(), false));
    }
    let Some(mut t) = text else { return false };
    let rect = Rect::from_center_size(c, Vec2::new((2.0 * r + 16.0).max(64.0), 20.0));
    let resp = ui.put(rect, egui::TextEdit::singleline(&mut t).id(id.with("te")).font(mono(11.0)).horizontal_align(egui::Align::Center).text_color(WHITE));
    if start {
        resp.request_focus();
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(t.chars().count()))));
            st.store(ui.ctx(), resp.id);
        }
    }
    let enter = ui.input(|i| i.key_pressed(Key::Enter));
    let esc = ui.input(|i| i.key_pressed(Key::Escape));
    if resp.lost_focus() || esc {
        if enter && !esc {
            if let Some(v) = p.string_to_normalized_value(&t) {
                set_norm(setter, p, v);
            }
        }
        ui.data_mut(|d| d.remove::<String>(tid));
        return false;
    }
    ui.data_mut(|d| d.insert_temp(tid, t));
    true
}

/// Flat knob: a thin ring, the value as a coloured arc, the value written in the middle and the
/// label under it. Drag up / down (Shift: fine), wheel; double-click: type a value; Ctrl-click:
/// default.
#[allow(clippy::too_many_arguments)]
fn fknob<P: Param>(ui: &mut Ui, setter: &ParamSetter, p: &P, label: &str, cell: Rect, tip: &str, bipolar: bool, r: f32, accent: Color32) -> egui::Response {
    let c = Pos2::new(cell.center().x, cell.top() + r + 4.0);
    let resp = ui.interact(Rect::from_center_size(c, Vec2::splat(2.0 * r + 6.0)), egui::Id::new(("fknob", p.name())), Sense::click_and_drag());
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    let norm = p.unmodulated_normalized_value();
    if resp.drag_started() {
        setter.begin_set_parameter(p);
    }
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let d = -resp.drag_delta().y / if fine { 1200.0 } else { 220.0 };
        setter.set_parameter_normalized(p, (norm + d).clamp(0.0, 1.0));
    }
    if resp.drag_stopped() {
        setter.end_set_parameter(p);
    }
    if resp.clicked() && ui.input(|i| i.modifiers.command) {
        set_norm(setter, p, p.default_normalized_value());
    }
    let editing = knob_text_edit(ui, setter, p, resp.id, c, r, resp.double_clicked());
    if resp.hovered() {
        let sc = ui.input(|i| i.smooth_scroll_delta.y);
        if sc != 0.0 {
            set_norm(setter, p, (norm + sc / 1500.0).clamp(0.0, 1.0));
        }
    }
    let norm = p.unmodulated_normalized_value();
    let painter = ui.painter();
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |t0: f32, t1: f32, rad: f32| -> Vec<Pos2> {
        let n = (((t1 - t0).abs() * 60.0).ceil() as usize).max(2);
        (0..=n)
            .map(|k| {
                let a = a0 + (t0 + (t1 - t0) * k as f32 / n as f32) * sweep;
                c + Vec2::new(a.cos(), a.sin()) * rad
            })
            .collect()
    };
    let hov = resp.hovered() || resp.dragged();
    painter.add(egui::Shape::line(arc(0.0, 1.0, r), PathStroke::new(3.0_f32, if hov { Color32::from_rgb(52, 56, 66) } else { Color32::from_rgb(40, 43, 51) })));
    let from = if bipolar { 0.5 } else { 0.0 };
    if (norm - from).abs() > 0.002 {
        painter.add(egui::Shape::line(arc(from.min(norm), from.max(norm), r), PathStroke::new(3.0_f32, accent)));
    }
    let a = a0 + norm * sweep;
    painter.circle_filled(c + Vec2::new(a.cos(), a.sin()) * r, 3.5, WHITE);
    let txt = p.normalized_value_to_string(norm, true);
    let size = if r >= 20.0 { 11.0 } else { 9.5 };
    let gw = ui.fonts(|f| f.layout_no_wrap(txt.clone(), mono(size), WHITE).size().x);
    let size = if gw > r * 1.7 { size * r * 1.7 / gw } else { size };
    if !editing {
        painter.text(c, Align2::CENTER_CENTER, txt, mono(size), WHITE);
    }
    painter.text(Pos2::new(c.x, c.y + r + 11.0), Align2::CENTER_CENTER, label, mono(9.0), Color32::from_rgb(130, 136, 148));
    resp
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
    fknob(ui, setter, &b.gain, "GAIN", cell, "Gain, ±30 dB.", true, r, col);
    let painter = ui.painter();
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let rr = r + 7.0;
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

/// what a click / drag at a display position would create (Pro-Q style): far left = low cut,
/// far right = high cut, along the bottom = notch, the curve's ends = shelves, else a bell
fn candidate(gr: Rect, pos: Pos2, gain: f32, on_curve_end: bool) -> (Shape, f32) {
    let u = (pos.x - gr.left()) / gr.width();
    if on_curve_end {
        return (if u < 0.5 { Shape::LowShelf } else { Shape::HighShelf }, gain);
    }
    if u < 0.045 {
        (Shape::LowCut, 0.0)
    } else if u > 0.955 {
        (Shape::HighCut, 0.0)
    } else if pos.y > gr.bottom() - 26.0 {
        (Shape::Notch, 0.0)
    } else {
        (Shape::Bell, gain)
    }
}

/// a new band of `shape`; Alt = dynamic, Alt + Shift = spectral
fn new_band(setter: &ParamSetter, p: &EqParams, shape: Shape, f: f32, g: f32, dynamic: bool, spectral: bool) -> Option<usize> {
    let i = p.bands.iter().position(|b| !b.used.value())?;
    let b = &p.bands[i];
    reset_band(setter, b);
    set_e(setter, &b.shape, shape);
    let f = match shape {
        Shape::LowCut => f.max(10.0).min(400.0),
        Shape::HighCut => f.max(2000.0),
        _ => f,
    };
    set_f(setter, &b.freq, f.clamp(10.0, 30000.0));
    if shape.has_gain() {
        set_f(setter, &b.gain, (g.clamp(-30.0, 30.0) * 10.0).round() / 10.0);
    }
    if shape == Shape::Notch {
        set_f(setter, &b.q, 4.0);
    }
    if (dynamic || spectral) && shape.has_gain() {
        set_f(setter, &b.dyn_range, if g > 0.0 { 6.0 } else { -6.0 });
        if spectral {
            set_b(setter, &b.spectral, true);
        }
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

/// flat text button for the thin bars (no box): bright when on, grey when off
fn lbtn(ui: &mut Ui, r: Rect, text: &str, on: bool, tip: &str) -> egui::Response {
    let resp = ui.allocate_rect(r, Sense::click());
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(4), Color32::from_white_alpha(14));
    }
    p.text(r.center(), Align2::CENTER_CENTER, text, mono(10.5), if on { WHITE } else { Color32::from_rgb(140, 146, 158) });
    resp
}

/// button: flat fill, 6 px radius, white when on
fn tbtn(ui: &mut Ui, r: Rect, text: &str, on: bool, tip: &str) -> egui::Response {
    let resp = ui.allocate_rect(r, Sense::click());
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect_filled(r, CornerRadius::same(6), if on { C_TXT } else if hov { C_BTN_H } else { C_BTN });
    if !text.is_empty() {
        let gw = ui.fonts(|f| f.layout_no_wrap(text.to_string(), mono(11.0), WHITE).size().x);
        let size = if gw > r.width() - 8.0 { (11.0 * (r.width() - 8.0) / gw).max(7.5) } else { 11.0 };
        p.text(r.center(), Align2::CENTER_CENTER, text, mono(size), if on { Color32::BLACK } else { C_TXT });
    }
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



/// a value in the parameter display: drag up / down or the wheel changes it, double-click types it
fn value_cell(ui: &mut Ui, r: Rect, id: egui::Id, text: &str, col: Color32) -> (egui::Response, f32) {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let painter = ui.painter();
    if resp.hovered() || resp.dragged() {
        painter.rect_filled(r, CornerRadius::same(3), Color32::from_white_alpha(14));
    }
    painter.text(r.center(), Align2::CENTER_CENTER, text, mono(10.5), col);
    let mut delta = 0.0;
    if resp.dragged() {
        delta -= resp.drag_delta().y;
    }
    if resp.hovered() {
        delta += ui.input(|i| i.smooth_scroll_delta.y) * 0.25;
    }
    if ui.input(|i| i.modifiers.shift) {
        delta *= 0.2;
    }
    (resp, delta)
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
            pbox: None,
            solo_drag: None,
            edit: None,
            pop: Pop::None,
            sketch_on: false,
            sketch: Vec::new(),
            learn: false,
            m_pre: vec![0.0; FIT_N],
            m_sc: vec![0.0; FIT_N],
            m_cnt: 0,
            cache: (0..NB).map(|_| (None, Vec::with_capacity(NPTS + 1))).collect(),
            undo: Vec::new(),
            zoom: (F_LO, F_HI),
            cache_zoom: (F_LO, F_HI),
            scale_drag: None,
            grab: 0,
            grab_t: -1.0,
            grab_pts: None,
            eat_click: false,
            freeze_hold: false,
            freeze_t: None,
            redo: Vec::new(),
            ab: [None, None],
            ab_cur: 0,
            last_snap_t: 0.0,
            pending_snap: true,
        },
        |ctx, _| eq_theme(ctx),
        move |ctx, setter, st| {
            ctx.request_repaint_after(Duration::from_millis(30));
            let p = &*params;
            let sh = &*shared;
            let fs = f32::from_bits(sh.fs.load(Ordering::Relaxed));
            let an_n = p.an_res.value().n();
            let fall = p.an_speed.value().fall();
            if !ctx.input(|i| i.pointer.any_down()) {
                st.freeze_hold = false;
            }
            let freeze = p.an_freeze.value() || st.freeze_hold;
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
            let now = ctx.input(|i| i.time);
            let released = ctx.input(|i| i.pointer.any_released() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: false, .. })) || i.smooth_scroll_delta.y != 0.0);
            if released {
                st.pending_snap = true;
                st.last_snap_t = now;
            }
            if st.pending_snap && now - st.last_snap_t > 0.35 && !ctx.input(|i| i.pointer.any_down()) {
                st.pending_snap = false;
                let s = snapshot(p);
                if !st.undo.last().is_some_and(|l| same_snap(l, &s)) {
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
                let pointer = ui.input(|i| i.pointer.hover_pos());
                let mods = ui.input(|i| i.modifiers);
                let byp = p.bypass.value();

                // ---------------- band data and curves (first: the display range follows them)
                let gr = rect(0.0, 0.0, BASE_W - METER_W, BAR_Y);
                let solo_now = sh.solo.load(Ordering::Relaxed);
                let views: Vec<BandView> = (0..NB).map(|i| view(p, sh, i)).collect();
                for i in 0..NB {
                    if !views[i].used {
                        st.sel[i] = false;
                        if st.focus == Some(i) {
                            st.focus = None;
                        }
                    }
                }
                // range: the chosen one, widened automatically when the curve goes outside it
                let set_rng = p.disp.value().db();
                let (zlo, zhi) = st.zoom;
                if st.cache_zoom != st.zoom {
                    st.cache_zoom = st.zoom;
                    for c in st.cache.iter_mut() {
                        c.0 = None;
                    }
                }
                let probe = Disp { g: gr, range: set_rng, an_range: 90.0, lo: zlo, hi: zhi };
                for i in 0..NB {
                    if views[i].used {
                        let spec = views[i].spec;
                        if st.cache[i].0 != Some(spec) {
                            let mut v = std::mem::take(&mut st.cache[i].1);
                            resp(&spec, &probe, &mut v);
                            st.cache[i] = (Some(spec), v);
                        }
                    }
                }
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
                        resp(&s2, &probe, &mut live);
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
                let peak = tot_a.iter().chain(tot_b.iter()).fold(0.0f32, |m, v| m.max(v.abs().min(40.0)));
                let rng = [3.0f32, 6.0, 12.0, 30.0].into_iter().find(|&r| r >= set_rng && r >= peak * 0.92).unwrap_or(30.0);
                let d = Disp { g: gr, range: rng, an_range: p.an_range.value().db(), lo: zlo, hi: zhi };
                let painter = ui.painter().clone();
                painter.rect_filled(gr, CornerRadius::ZERO, Color32::from_rgb(8, 9, 12));
                let clip = painter.with_clip_rect(gr.shrink(1.0));
                grad(&clip, gr, Color32::from_rgb(20, 22, 28), Color32::from_rgb(11, 12, 15));
                let piano = p.piano.value();
                for f in [20.0f32, 30.0, 40.0, 50.0, 60.0, 80.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 800.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 8000.0, 10000.0, 20000.0] {
                    let major = matches!(f as i32, 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000);
                    clip.vline(d.fx(f), gr.top()..=gr.bottom(), Stroke::new(1.0_f32, Color32::from_white_alpha(if major { 10 } else { 4 })));
                }
                let step = match rng as i32 {
                    3 => 1.0,
                    6 => 2.0,
                    12 => 3.0,
                    _ => 6.0,
                };
                let mut v = -(rng / step).floor() * step;
                while v <= rng + 0.01 {
                    let y = d.dy(v);
                    clip.hline(gr.left()..=gr.right(), y, Stroke::new(1.0_f32, Color32::from_white_alpha(if v == 0.0 { 24 } else { 6 })));
                    clip.text(Pos2::new(gr.right() - 8.0, y - 1.0), Align2::RIGHT_BOTTOM, if v == 0.0 { "0".into() } else { format!("{:+.0}", v) }, mono(9.5), if v == 0.0 { C_TXT } else { C_TXT2 });
                    v += step;
                }
                let mut a = 0.0f32;
                while a >= -d.an_range + 0.1 {
                    if a < -1.0 {
                        clip.text(Pos2::new(gr.left() + 8.0, d.sy(a)), Align2::LEFT_CENTER, format!("{:.0}", a), mono(8.5), Color32::from_rgb(78, 84, 98));
                    }
                    a -= if d.an_range > 90.0 { 20.0 } else { 10.0 };
                }
                // analyzer
                let tilt = p.an_tilt.value().db();
                let to_pts = |v: &[(f32, f32)]| -> Vec<Pos2> { v.iter().map(|&(f, db)| Pos2::new(d.fx(f), d.sy(db))).collect() };
                let post_pts = if p.an_post.value() { Some(st.post.points(&d, fs, tilt, 320)) } else { None };
                let pre_pts = if p.an_pre.value() { Some(st.pre.points(&d, fs, tilt, 320)) } else { None };
                let sc_pts = if p.an_sc.value() { Some(st.sc.points(&d, fs, tilt, 320)) } else { None };
                let (mut post_pts, mut pre_pts) = (post_pts, pre_pts);
                if st.grab > 0 && st.grab_pts.is_some() && (post_pts.is_some() || pre_pts.is_some()) {
                    if post_pts.is_some() {
                        post_pts = st.grab_pts.clone();
                    } else {
                        pre_pts = st.grab_pts.clone();
                    }
                } else if st.grab > 0 {
                    st.grab = 0;
                    st.grab_pts = None;
                }
                if let Some(pp) = &pre_pts {
                    let pts = to_pts(pp);
                    grad_fill(&clip, &smooth_line(&pts, 2), gr.bottom(), Color32::from_rgb(120, 130, 150), 30, 2);
                    clip.add(egui::Shape::line(smooth_line(&pts, 2), PathStroke::new(1.0_f32, Color32::from_rgba_unmultiplied(150, 160, 180, 70))));
                }
                if let Some(pp) = &post_pts {
                    clip.add(egui::Shape::line(smooth_line(&to_pts(pp), 2), PathStroke::new(1.2_f32, Color32::from_rgb(150, 160, 178))));
                }
                if let Some(sp) = &sc_pts {
                    clip.add(egui::Shape::line(smooth_line(&to_pts(sp), 2), PathStroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 150))));
                    if p.an_coll.value() {
                        if let Some(m) = post_pts.as_ref().or(pre_pts.as_ref()) {
                            for (a, b) in m.iter().zip(sp.iter()) {
                                if a.1 > -d.an_range + 20.0 && b.1 > -d.an_range + 20.0 && (a.1 - b.1).abs() < 8.0 {
                                    let x = d.fx(a.0);
                                    clip.line_segment([Pos2::new(x, d.sy(a.1.min(b.1))), Pos2::new(x, gr.bottom())], Stroke::new(3.5_f32, Color32::from_rgba_unmultiplied(220, 50, 50, 40)));
                                }
                            }
                        }
                    }
                }
                if sh.sp_on.load(Ordering::Relaxed) {
                    let spn = sh.sp_n.load(Ordering::Relaxed) as usize;
                    let mut pts = Vec::with_capacity(321);
                    for k in 0..=320 {
                        let f = d.lo * (d.hi.min(fs * 0.5) / d.lo).powf(k as f32 / 320.0);
                        let bin = ((f * spn as f32 / fs) as usize).min(spn / 2);
                        pts.push(Pos2::new(d.fx(f), d.dy(Shared::get(&sh.sp_show[bin]).clamp(-rng * 1.2, rng * 1.2))));
                    }
                    fill_under(&clip, &pts, d.dy(0.0), Color32::from_rgba_unmultiplied(226, 176, 88, 40));
                    clip.add(egui::Shape::line(pts, PathStroke::new(1.2_f32, C_DYN)));
                }
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
                let zoomed = (zhi / zlo) < 300.0;
                for f in [20.0f32, 30.0, 40.0, 50.0, 60.0, 80.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 800.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 8000.0, 10000.0, 20000.0] {
                    let major = matches!(f as i32, 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000);
                    if !major && !zoomed {
                        continue;
                    }
                    let lab = if f >= 1000.0 { format!("{}k", (f / 1000.0) as i32) } else { format!("{}", f as i32) };
                    clip.text(Pos2::new(d.fx(f), gr.bottom() - 9.0), Align2::CENTER_CENTER, lab, mono(9.5), MID);
                }
                // the cached curves were made with the chosen range only for x; y is mapped now
                let xs: Vec<f32> = (0..=NPTS).map(|k| gr.left() + gr.width() * k as f32 / NPTS as f32).collect();
                let zero = d.dy(0.0);
                let clampc = |v: f32| v.clamp(-rng * 1.25, rng * 1.25);
                if let Some(i) = st.focus {
                    let b = &p.bands[i];
                    if views[i].used && views[i].dynamic && b.sc_free.value() {
                        let r = Rect::from_min_max(Pos2::new(d.fx(b.sc_lo.value()), gr.top()), Pos2::new(d.fx(b.sc_hi.value()), gr.bottom()));
                        clip.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(226, 176, 88, 10));
                        clip.vline(r.left(), gr.top()..=gr.bottom(), Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 90)));
                        clip.vline(r.right(), gr.top()..=gr.bottom(), Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 90)));
                    }
                }
                let soloing = solo_now >= 0 && (solo_now as usize) < NB && views[solo_now as usize].used;
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used {
                        continue;
                    }
                    let col = band_col(i);
                    let hot = st.focus == Some(i) || st.hover == Some(i) || st.sel[i] || st.pbox.is_some_and(|q| q.0 == i);
                    let dim = soloing && solo_now as usize != i;
                    let pts: Vec<Pos2> = xs.iter().zip(st.cache[i].1.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                    let alpha = if !bv.on || dim { 40 } else if hot { 255 } else { 170 };
                    if bv.on && !dim {
                        grad_fill(&clip, &pts, zero, col, if hot { 120 } else { 62 }, if hot { 14 } else { 4 });
                    }
                    clip.add(egui::Shape::line(pts, PathStroke::new(if hot { 2.0_f32 } else { 1.3 }, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha))));
                    if bv.dynamic && bv.on && hot {
                        let mut s2 = bv.spec;
                        s2.gain_db = (s2.gain_db + bv.range as f64).clamp(-30.0, 30.0);
                        let mut v2 = Vec::with_capacity(NPTS + 1);
                        resp(&s2, &d, &mut v2);
                        let top: Vec<Pos2> = xs.iter().zip(v2.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                        // the dynamic range as a filled area between the static curve and gain + range
                        let base: Vec<Pos2> = xs.iter().zip(st.cache[i].1.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                        for k in 0..base.len().saturating_sub(1) {
                            let mut m = egui::Mesh::default();
                            let c = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 60);
                            for q in [base[k], base[k + 1], top[k + 1], top[k]] {
                                m.colored_vertex(q, c);
                            }
                            m.add_triangle(0, 1, 2);
                            m.add_triangle(0, 2, 3);
                            clip.add(egui::Shape::mesh(m));
                        }
                        clip.add(egui::Shape::line(top, PathStroke::new(1.0_f32, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 200))));
                    }
                }
                let line_of = |t: &[f32]| -> Vec<Pos2> { xs.iter().zip(t.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect() };
                let curve_col = if byp || soloing { DIM } else { WHITE };
                if split {
                    let has_ms = views.iter().any(|v| v.used && v.on && matches!(v.place, Place::Mid | Place::Side));
                    clip.add(egui::Shape::line(line_of(&tot_b), PathStroke::new(1.8_f32, Color32::from_rgb(120, 200, 230))));
                    clip.text(Pos2::new(gr.left() + 10.0, gr.top() + 14.0), Align2::LEFT_CENTER, if has_ms { "— MID / LEFT" } else { "— LEFT" }, mono(9.5), WHITE);
                    clip.text(Pos2::new(gr.left() + 10.0, gr.top() + 28.0), Align2::LEFT_CENTER, if has_ms { "— SIDE / RIGHT" } else { "— RIGHT" }, mono(9.5), Color32::from_rgb(120, 200, 230));
                }
                let main = line_of(&tot_a);
                if !split {
                    grad_fill(&clip, &main, zero, WHITE, 16, 0);
                }
                // one crisp line, no glow around it (the halo made it look soft)
                clip.add(egui::Shape::line(main.clone(), PathStroke::new(1.8_f32, curve_col)));

                // ---------------- geometry of the overlays (they take the mouse first)
                let node_pos = |i: usize| -> Pos2 {
                    let bv = &views[i];
                    let y = if bv.spec.shape.has_gain() { bv.spec.gain_db as f32 } else { 0.0 };
                    Pos2::new(d.fx(bv.spec.freq as f32).clamp(gr.left() + 6.0, gr.right() - 6.0), d.dy(y.clamp(-rng, rng)).clamp(gr.top() + 6.0, gr.bottom() - 6.0))
                };
                let panel_w = |i: usize| if st.dyn_open && views[i].spec.shape.has_gain() { 932.0 } else { 552.0 };
                let panel_rect = st.focus.filter(|&i| views[i].used).map(|i| {
                    let w = panel_w(i);
                    let h = 120.0;
                    let cx = node_pos(i).x.clamp(gr.left() + w * 0.5 + 6.0, gr.right() - w * 0.5 - 6.0);
                    Rect::from_min_size(Pos2::new(cx - w * 0.5, gr.bottom() - h - 24.0), Vec2::new(w, h))
                });
                let ana_btn = rect(606.0, BAR_Y + 4.0, 150.0, 24.0);
                let ana_rect = if st.pop != Pop::None { Some(Rect::from_min_size(Pos2::new(ana_btn.left() - 140.0, ana_btn.top() - 222.0), Vec2::new(440.0, 214.0))) } else { None };
                // the parameter display box of the band under the mouse (beside its dot)
                let pbox_rect = |i: usize| -> Rect {
                    let np = node_pos(i);
                    let (w, h) = (196.0, 50.0);
                    let x = if np.x + 18.0 + w < gr.right() - 4.0 { np.x + 16.0 } else { np.x - 16.0 - w };
                    let y = (np.y - h * 0.5).clamp(gr.top() + 4.0, gr.bottom() - h - 4.0);
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
                };
                let in_overlay = |pp: Pos2| panel_rect.is_some_and(|r| r.contains(pp)) || ana_rect.is_some_and(|r| r.contains(pp));

                // ---------------- interaction: background, then dots (dots win)
                let bg = ui.interact(gr, egui::Id::new("eq_bg"), Sense::click_and_drag());
                let any_sel = st.sel.iter().any(|&s| s);
                // where the overall curve is under the mouse
                let curve_y_at = |x: f32| -> f32 {
                    let k = (((x - gr.left()) / gr.width()) * NPTS as f32).round().clamp(0.0, NPTS as f32) as usize;
                    d.dy(clampc(tot_a[k]))
                };
                let near_curve = |pp: Pos2| (pp.y - curve_y_at(pp.x)).abs() < 7.0;
                let curve_end = |pp: Pos2| {
                    let u = (pp.x - gr.left()) / gr.width();
                    !(0.06..=0.94).contains(&u) && near_curve(pp)
                };
                // the band whose own curve or coloured area is under the mouse (click selects it, the
                // wheel changes its Q), not where the overall curve is (dragging that makes a band)
                let area_band: Option<usize> = pointer.filter(|pp| gr.contains(*pp) && !in_overlay(*pp) && pp.y > gr.top() + 34.0 && !near_curve(*pp)).and_then(|pp| {
                    let k = (((pp.x - gr.left()) / gr.width()) * NPTS as f32).round().clamp(0.0, NPTS as f32) as usize;
                    let mut best: Option<(usize, f32)> = None;
                    for i in 0..NB {
                        if !views[i].used || !views[i].on || st.cache[i].0.is_none() {
                            continue;
                        }
                        let v = clampc(st.cache[i].1[k]);
                        if v.abs() < 1.0 {
                            continue;
                        }
                        let y = d.dy(v);
                        let on_line = (pp.y - y).abs() < 6.0;
                        let inside = (pp.y - zero) * (y - zero) > 0.0 && (pp.y - zero).abs() < (y - zero).abs();
                        if on_line || inside {
                            let score = if on_line { 1000.0 } else { v.abs() };
                            if best.is_none_or(|b| score > b.1) {
                                best = Some((i, score));
                            }
                        }
                    }
                    best.map(|b| b.0)
                });
                // spectrum grab (Pro-Q style): rest the mouse in the spectrum for a second (or click and
                // hold for a permanent grab): the spectrum freezes, the bands dim, the main peaks get
                // labels and a peak can be dragged to make a bell there
                let now_t = ui.input(|i| i.time);
                let any_down = ui.input(|i| i.pointer.any_down());
                let spts = post_pts.as_ref().or(pre_pts.as_ref());
                let line_y = |x: f32| -> Option<f32> {
                    let v = spts?;
                    let f = d.xf(x);
                    let k = v.partition_point(|q| q.0 < f).clamp(1, v.len() - 1);
                    let (a, b) = (v[k - 1], v[k]);
                    let t = if b.0 > a.0 { ((f / a.0).ln() / (b.0 / a.0).ln()).clamp(0.0, 1.0) } else { 0.0 };
                    Some(d.sy(a.1 + (b.1 - a.1) * t))
                };
                let scale_strip = Rect::from_min_max(Pos2::new(gr.left(), gr.bottom() - 16.0), gr.max);
                let near_node_at = |pp: Pos2| (0..NB).any(|i| views[i].used && node_pos(i).distance(pp) < 16.0);
                let in_spectrum = |pp: Pos2| gr.contains(pp) && !in_overlay(pp) && !scale_strip.contains(pp) && pp.y > gr.top() + 34.0 && line_y(pp.x).is_some_and(|ly| pp.y > ly + 3.0) && !near_node_at(pp) && !near_curve(pp);
                let mut peaks: Vec<(f32, f32)> = Vec::new();
                let mut grab: Option<(f32, Pos2)> = None;
                if p.an_grab.value() && !st.sketch_on && st.edit.is_none() && spts.is_some() {
                    // enter: resting in the spectrum (no button down)
                    if st.grab == 0 {
                        match pointer {
                            Some(pp) if !any_down && in_spectrum(pp) => {
                                if st.grab_t < 0.0 {
                                    st.grab_t = now_t;
                                } else if now_t - st.grab_t > 1.0 {
                                    st.grab = 1;
                                    st.grab_pts = spts.cloned();
                                }
                            }
                            _ => st.grab_t = -1.0,
                        }
                        // click and hold (still) in the spectrum: permanent
                        if bg.is_pointer_button_down_on() && st.drag.is_none() {
                            let (t0, o0) = ui.input(|i| (i.pointer.press_start_time(), i.pointer.press_origin()));
                            if let (Some(t0), Some(o0), Some(pp)) = (t0, o0, pointer) {
                                if pp.distance(o0) < 4.0 && in_spectrum(o0) && now_t - t0 > 0.55 {
                                    st.grab = 2;
                                    st.grab_pts = spts.cloned();
                                    st.eat_click = true;
                                }
                            }
                        }
                    }
                    if st.grab > 0 {
                        // the main peaks of the frozen spectrum
                        let v = spts.unwrap();
                        let mut cand: Vec<(f32, f32)> = Vec::new();
                        for k in 4..v.len().saturating_sub(4) {
                            let y = v[k].1;
                            let lo = (k.saturating_sub(12)..(k + 13).min(v.len())).map(|j| v[j].1).fold(f32::MAX, f32::min);
                            if (k - 4..=k + 4).all(|j| v[j].1 <= y) && y - lo > 6.0 && y > -d.an_range + 12.0 {
                                cand.push(v[k]);
                            }
                        }
                        cand.sort_by(|a, b| b.1.total_cmp(&a.1));
                        for c in cand {
                            let x = d.fx(c.0);
                            if x > gr.left() + 10.0 && x < gr.right() - 10.0 && peaks.iter().all(|q| (d.fx(q.0) - x).abs() > 44.0) && peaks.len() < 8 {
                                peaks.push(c);
                            }
                        }
                        // the target: a labelled peak, or the spectrum line itself
                        if st.drag.is_none() {
                            if let Some(pp) = pointer.filter(|pp| gr.contains(*pp) && !in_overlay(*pp)) {
                                for q in &peaks {
                                    let qp = Pos2::new(d.fx(q.0), d.sy(q.1));
                                    if (qp.x - pp.x).abs() < 14.0 && (-28.0..40.0).contains(&(pp.y - qp.y)) {
                                        grab = Some((q.0, qp));
                                        break;
                                    }
                                }
                                if grab.is_none() {
                                    if let Some(ly) = line_y(pp.x) {
                                        if (pp.y - ly).abs() < 8.0 {
                                            grab = Some((d.xf(pp.x), Pos2::new(pp.x, ly)));
                                        }
                                    }
                                }
                            }
                        }
                        // leave the temporary mode: mouse above the spectrum or out of the display
                        if st.grab == 1 && st.drag.is_none() && !any_down && grab.is_none() {
                            let out = pointer.is_none_or(|pp| !gr.contains(pp) || in_overlay(pp) || line_y(pp.x).is_some_and(|ly| pp.y < ly - 24.0));
                            if out {
                                st.grab = 0;
                                st.grab_pts = None;
                                st.grab_t = -1.0;
                            }
                        }
                    }
                } else {
                    st.grab = 0;
                    st.grab_pts = None;
                    st.grab_t = -1.0;
                }
                let mut new_hover = None;
                let mut node_resps: Vec<(usize, egui::Response)> = Vec::new();
                for i in 0..NB {
                    if !views[i].used {
                        continue;
                    }
                    let r = ui.interact(Rect::from_center_size(node_pos(i), Vec2::splat(22.0)), egui::Id::new(("eqnode", i)), Sense::click_and_drag());
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
                        let r = ui.interact(Rect::from_center_size(hp, Vec2::new(26.0, 12.0)), egui::Id::new(("eqrange", i)), Sense::drag());
                        range_handle = Some((i, r));
                    }
                }
                st.hover = new_hover.or(if st.drag.is_none() && st.grab == 0 && st.rubber.is_none() { area_band } else { None });
                let snap = |f: f32| if piano { snap_note(f) } else { f };
                let mut used_sorted: Vec<usize> = (0..NB).filter(|&k| views[k].used).collect();
                used_sorted.sort_by(|&a, &c| views[a].spec.freq.total_cmp(&views[c].spec.freq));
                let mut open_edit: Option<usize> = None;

                for (i, r) in node_resps.iter() {
                    let i = *i;
                    let b = &p.bands[i];
                    if r.clicked() {
                        if mods.alt && mods.command {
                            // cycle the shape
                            let k = Shape::ALL.iter().position(|s| *s == b.shape.value()).unwrap_or(0);
                            set_e(setter, &b.shape, Shape::ALL[(k + 1) % Shape::ALL.len()]);
                        } else if mods.alt && mods.shift {
                            // cycle the slope
                            let shp = b.shape.value();
                            let list: Vec<Slope> = Slope::ALL.iter().copied().filter(|s| s.for_shape(shp) == *s).collect();
                            let k = list.iter().position(|s| *s == b.slope.value().for_shape(shp)).unwrap_or(0);
                            set_e(setter, &b.slope, list[(k + 1) % list.len()]);
                        } else if mods.alt {
                            set_b(setter, &b.on, !b.on.value());
                        } else if mods.command {
                            st.sel[i] = !st.sel[i];
                            st.focus = Some(i);
                        } else if mods.shift {
                            // consecutive range (by frequency) from the focused band
                            if let Some(f0) = st.focus {
                                let a = used_sorted.iter().position(|&k| k == f0).unwrap_or(0);
                                let c = used_sorted.iter().position(|&k| k == i).unwrap_or(0);
                                for &k in &used_sorted[a.min(c)..=a.max(c)] {
                                    st.sel[k] = true;
                                }
                            }
                            st.sel[i] = true;
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
                        open_edit = Some(i);
                    }
                    if r.drag_started() {
                        if !st.sel[i] {
                            if !mods.command {
                                st.sel = [false; NB];
                            }
                            st.sel[i] = true;
                        }
                        st.focus = Some(i);
                        let bands: Vec<(usize, f32, f32, f32)> = (0..NB).filter(|&k| st.sel[k] && views[k].used).map(|k| (k, p.bands[k].freq.value(), p.bands[k].gain.value(), p.bands[k].q.value())).collect();
                        for (k, _, _, _) in &bands {
                            begin(setter, &p.bands[*k]);
                            setter.begin_set_parameter(&p.bands[*k].q);
                        }
                        st.drag = Some(Drag { start: r.interact_pointer_pos().unwrap_or(node_pos(i)), bands, kind: DragKind::Move, axis: 0 });
                    }
                    if r.dragged() {
                        if let (Some(dr), Some(pp)) = (st.drag.as_mut(), r.interact_pointer_pos()) {
                            let fine = if mods.shift { 0.15 } else { 1.0 };
                            let mut dx = (pp.x - dr.start.x) * fine;
                            let mut dyv = (pp.y - dr.start.y) * fine;
                            if mods.alt {
                                if dr.axis == 0 && (dx.abs() > 4.0 || dyv.abs() > 4.0) {
                                    dr.axis = if dx.abs() > dyv.abs() { 1 } else { 2 };
                                }
                                if dr.axis == 1 {
                                    dyv = 0.0;
                                } else if dr.axis == 2 {
                                    dx = 0.0;
                                }
                            } else {
                                dr.axis = 0;
                            }
                            let ratio = (d.xf(gr.left() + gr.width() * 0.5 + dx) / d.xf(gr.left() + gr.width() * 0.5)).max(1e-3);
                            let one = dr.bands.len() == 1;
                            for &(k, f0, g0, q0) in &dr.bands {
                                let bk = &p.bands[k];
                                let nf = (f0 * ratio).clamp(10.0, 30000.0);
                                setter.set_parameter(&bk.freq, if one { snap(nf) } else { nf });
                                if mods.command {
                                    // Ctrl + vertical: Q
                                    setter.set_parameter(&bk.q, (q0 * 2f32.powf(-dyv / 60.0)).clamp(0.025, 40.0));
                                } else if bk.shape.value().has_gain() {
                                    let dg = -dyv / (gr.height() * 0.44) * rng;
                                    setter.set_parameter(&bk.gain, ((g0 + dg) * 100.0).round() / 100.0);
                                }
                            }
                        }
                    }
                    if r.drag_stopped() {
                        if let Some(dr) = st.drag.take() {
                            for (k, _, _, _) in &dr.bands {
                                end(setter, &p.bands[*k]);
                                setter.end_set_parameter(&p.bands[*k].q);
                            }
                        }
                    }
                    r.context_menu(|ui| band_menu(ui, setter, p, i, &views));
                }
                // wheel: over a dot (or anywhere over the display for the selected bands)
                let wheel = ui.input(|i| i.smooth_scroll_delta.y);
                if wheel != 0.0 && pointer.is_some_and(|pp| gr.contains(pp) && !in_overlay(pp) && !st.pbox.is_some_and(|(k, _)| pbox_rect(k).contains(pp))) {
                    let targets: Vec<usize> = if let Some(h) = st.hover { vec![h] } else { (0..NB).filter(|&k| st.sel[k] && views[k].used).collect() };
                    let fine = if mods.shift { 0.2 } else { 1.0 };
                    for k in targets {
                        let b = &p.bands[k];
                        let shp = b.shape.value();
                        if mods.alt && mods.command && shp.has_gain() {
                            // linked: gain goes one way, the dynamic range the other
                            let dv = wheel / 30.0 * fine;
                            set_f(setter, &b.gain, (b.gain.value() + dv).clamp(-30.0, 30.0));
                            set_f(setter, &b.dyn_range, (b.dyn_range.value() - dv).clamp(-30.0, 30.0));
                        } else if mods.alt && shp.has_gain() {
                            set_f(setter, &b.dyn_range, (b.dyn_range.value() + wheel / 30.0 * fine).clamp(-30.0, 30.0));
                        } else if mods.command && shp.has_gain() {
                            set_f(setter, &b.gain, (b.gain.value() + wheel / 30.0 * fine).clamp(-30.0, 30.0));
                        } else if matches!(shp, Shape::LowCut | Shape::HighCut) && !mods.shift {
                            // stepped slope
                            if wheel.abs() > 8.0 {
                                let list: Vec<Slope> = Slope::ALL.iter().copied().filter(|s| s.for_shape(shp) == *s).collect();
                                let cur = list.iter().position(|s| *s == b.slope.value().for_shape(shp)).unwrap_or(0) as i32;
                                let nk = (cur + if wheel > 0.0 { 1 } else { -1 }).clamp(0, list.len() as i32 - 1) as usize;
                                set_e(setter, &b.slope, list[nk]);
                            }
                        } else if shp.has_q() {
                            set_norm(setter, &b.q, b.q.unmodulated_normalized_value() + wheel / 1500.0 * fine);
                        }
                    }
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

                // background
                let bg_pos = bg.interact_pointer_pos().filter(|pp| !in_overlay(*pp));
                // a press on the frequency scale zooms / pans (handled below), it makes no band
                let on_scale = (bg.is_pointer_button_down_on() && ui.input(|i| i.pointer.press_origin()).is_some_and(|o| scale_strip.contains(o)))
                    || ((bg.clicked() || bg.double_clicked()) && bg.interact_pointer_pos().is_some_and(|o| scale_strip.contains(o)))
                    || st.scale_drag.is_some();
                let dyn_mod = mods.alt;
                let spec_mod = mods.alt && mods.shift;
                let mut created: Option<usize> = None;
                if st.sketch_on {
                    if bg.drag_started() {
                        st.sketch.clear();
                    }
                    if bg.dragged() {
                        if let Some(pp) = bg_pos.or(pointer) {
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
                            let cur: f32 = (0..NB).filter(|&i| views[i].used && views[i].on).map(|i| analog(&views[i].spec).db(fit_f(j) as f64) as f32).sum();
                            r[j] = want - cur;
                        }
                        if j1 > j0 {
                            let free = free_bands(p);
                            for ((f, g, q), &k) in fit_bells(&mut r, j0, j1, free.len().min(10)).iter().zip(free.iter()) {
                                put_bell(setter, &p.bands[k], *f, *g, *q);
                            }
                        }
                    }
                } else if let Some(pp) = bg_pos.filter(|_| !on_scale) {
                    // click: nothing selected -> new band here; with a selection -> deselect.
                    // double-click / Ctrl-click always make a band.
                    let make = |setter: &ParamSetter, pos: Pos2, end: bool| {
                        let (shape, g) = candidate(gr, pos, d.yd(pos.y), end);
                        new_band(setter, p, shape, snap(d.xf(pos.x)), g, dyn_mod, spec_mod)
                    };
                    if st.eat_click {
                        // this press switched the spectrum grab on: no band, no deselect
                    } else if st.grab == 2 && bg.clicked() && grab.is_none() {
                        st.grab = 0;
                        st.grab_pts = None;
                    } else if bg.double_clicked() || (bg.clicked() && mods.command) {
                        if !(bg.double_clicked() && created.is_some()) {
                            created = make(setter, pp, false);
                        }
                    } else if bg.clicked() && area_band.is_some() && !mods.alt {
                        let k = area_band.unwrap();
                        if mods.command {
                            st.sel[k] = !st.sel[k];
                        } else {
                            st.sel = [false; NB];
                            st.sel[k] = true;
                        }
                        st.focus = Some(k);
                    } else if bg.clicked() {
                        if any_sel {
                            st.sel = [false; NB];
                            st.focus = None;
                            st.pop = Pop::None;
                        } else {
                            created = make(setter, pp, false);
                        }
                    }
                    if bg.drag_started() && !st.eat_click {
                        // where the button went down, not where the drag was recognised
                        let start = ui.input(|i| i.pointer.press_origin()).unwrap_or(pp);
                        if let Some((gf, _)) = grab {
                            created = new_band(setter, p, Shape::Bell, snap(gf), 0.0, dyn_mod, spec_mod);
                            // Q from the peak's width: where the spectrum is 6 dB below the peak
                            if let (Some(i), Some(v)) = (created, spts) {
                                let k = v.partition_point(|q| q.0 < gf).min(v.len() - 1);
                                let top = v[k].1;
                                let (mut a, mut b) = (k, k);
                                while a > 0 && v[a].1 > top - 6.0 {
                                    a -= 1;
                                }
                                while b + 1 < v.len() && v[b].1 > top - 6.0 {
                                    b += 1;
                                }
                                let bw = (v[b].0 - v[a].0).max(1.0);
                                set_f(setter, &p.bands[i].q, (gf / bw).clamp(0.5, 12.0));
                            }
                        } else if near_curve(start) {
                            // dragging the curve makes a band (a shelf at its ends)
                            let (shape, _) = candidate(gr, start, 0.0, curve_end(start));
                            let shape = if matches!(shape, Shape::LowCut | Shape::HighCut | Shape::Notch) { Shape::Bell } else { shape };
                            created = new_band(setter, p, shape, snap(d.xf(start.x)), 0.0, dyn_mod, spec_mod);
                        } else {
                            st.rubber = Some(start);
                        }
                        if let Some(i) = created {
                            setter.begin_set_parameter(&p.bands[i].gain);
                            st.drag = Some(Drag { start, bands: vec![(i, d.xf(start.x), 0.0, 1.0)], kind: DragKind::Create, axis: 0 });
                        }
                    }
                }
                if let Some(i) = created {
                    st.sel = [false; NB];
                    st.sel[i] = true;
                    st.focus = Some(i);
                }
                if bg.dragged() {
                    if let (Some(dr), Some(pp)) = (st.drag.as_ref(), pointer) {
                        if dr.kind == DragKind::Create {
                            if let Some(&(k, _, g0, _)) = dr.bands.first() {
                                let dg = -(pp.y - dr.start.y) / (gr.height() * 0.44) * rng;
                                setter.set_parameter(&p.bands[k].gain, ((g0 + dg) * 10.0).round() / 10.0);
                            }
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
                                st.sel[i] = rr.contains(node_pos(i)) || (mods.command && st.sel[i]);
                            }
                        }
                    }
                }
                if bg.drag_stopped() && st.grab == 1 {
                    // a temporary grab ends with the drag
                    st.grab = 0;
                    st.grab_pts = None;
                    st.grab_t = -1.0;
                }
                if !any_down {
                    st.eat_click = false;
                }
                if bg.drag_stopped() {
                    if let Some(dr) = st.drag.take() {
                        for (k, _, _, _) in &dr.bands {
                            setter.end_set_parameter(&p.bands[*k].gain);
                        }
                    }
                    if st.rubber.take().is_some() {
                        st.focus = (0..NB).find(|&i| st.sel[i]);
                    }
                }
                // preview of the band a click / drag would make
                if !st.sketch_on && st.drag.is_none() && st.rubber.is_none() && st.hover.is_none() && st.edit.is_none() && (st.grab == 0 || grab.is_some()) {
                    if let Some(pp) = pointer.filter(|pp| gr.contains(*pp) && !scale_strip.contains(*pp) && !in_overlay(*pp) && !st.pbox.is_some_and(|(i, _)| pbox_rect(i).contains(*pp))) {
                        let end = curve_end(pp);
                        let (shape, g) = if grab.is_some() { (Shape::Bell, 0.0) } else { candidate(gr, pp, if near_curve(pp) { 0.0 } else { d.yd(pp.y) }, end) };
                        let f = grab.map(|g| g.0).unwrap_or_else(|| d.xf(pp.x));
                        let spec = Spec { shape, slope: Slope::S12, freq: f as f64, gain_db: g.clamp(-30.0, 30.0) as f64, q: if shape == Shape::Notch { 4.0 } else { 1.0 }, gq: false };
                        let mut v = Vec::with_capacity(NPTS + 1);
                        resp(&spec, &d, &mut v);
                        let pts: Vec<Pos2> = xs.iter().zip(v.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                        let col = Color32::from_white_alpha(if any_sel { 34 } else { 60 });
                        if any_sel {
                            clip.add(egui::Shape::dashed_line(&pts, Stroke::new(1.0_f32, col), 5.0, 4.0));
                        } else {
                            clip.add(egui::Shape::line(pts, PathStroke::new(1.0_f32, col)));
                        }
                    }
                }
                let typing = ui.ctx().wants_keyboard_input();
                if !typing {
                    // Ctrl / Cmd / Alt + Z: undo; with Shift (or Ctrl + Y): redo
                    // the modifiers of the key event itself (a quick Alt+Z can release Alt in the same frame)
                    let keys: Vec<(Key, egui::Modifiers)> = ui.input(|i| i.events.iter().filter_map(|e| if let egui::Event::Key { key, pressed: true, modifiers, .. } = e { Some((*key, *modifiers)) } else { None }).collect());
                    for (k, m) in keys {
                        if k == Key::Z && (m.command || m.alt) {
                            if m.shift {
                                do_redo(st, setter, p);
                            } else {
                                do_undo(st, setter, p);
                            }
                        } else if k == Key::Y && m.command {
                            do_redo(st, setter, p);
                        }
                    }
                }
                if !typing && st.edit.is_none() && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
                    for i in 0..NB {
                        if st.sel[i] {
                            delete_band(setter, &p.bands[i]);
                            st.sel[i] = false;
                        }
                    }
                    st.focus = None;
                }
                // right-click on the background: copy all / paste
                bg.context_menu(|ui| {
                    let t = |s: &str| egui::RichText::new(s.to_string()).font(mono(11.5));
                    if ui.button(t("Copy all bands")).clicked() {
                        if let Ok(mut c) = CLIP.lock() {
                            *c = (0..NB).filter(|&k| views[k].used).map(|k| copy_band(p, k)).collect();
                        }
                        ui.close_menu();
                    }
                    let n = CLIP.lock().map(|c| c.len()).unwrap_or(0);
                    if ui.add_enabled(n > 0, egui::Button::new(t(&format!("Paste {n} band(s)")))).clicked() {
                        if let Ok(c) = CLIP.lock() {
                            for (vals, k) in c.iter().zip(free_bands(p)) {
                                paste_band(setter, p, k, vals);
                            }
                        }
                        ui.close_menu();
                    }
                    if ui.button(t("Delete all bands")).clicked() {
                        for k in 0..NB {
                            if views[k].used {
                                delete_band(setter, &p.bands[k]);
                            }
                        }
                        ui.close_menu();
                    }
                });
                if !views.iter().any(|v| v.used) && !st.sketch_on && st.grab == 0 {
                    clip.text(gr.center() - Vec2::new(0.0, 60.0), Align2::CENTER_CENTER, "click to add a band   ·   drag the line to shape it   ·   Alt: dynamic, Alt + Shift: spectral", mono(11.0), DIM);
                    clip.text(gr.center() - Vec2::new(0.0, 42.0), Align2::CENTER_CENTER, "far left / right: cuts   ·   line ends: shelves   ·   bottom edge: notch   ·   rest on the spectrum to grab peaks", mono(10.0), DIM);
                }
                if st.sketch_on {
                    clip.text(Pos2::new(gr.center().x, gr.top() + 16.0), Align2::CENTER_CENTER, "EQ SKETCH: draw the curve you want, bands are added to follow it", mono(10.5), Color32::from_rgb(120, 200, 255));
                }
                // ---------------- dots
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used {
                        continue;
                    }
                    let pos = node_pos(i);
                    let col = band_col(i);
                    let dim = soloing && solo_now as usize != i;
                    let hot = st.hover == Some(i);
                    let selected = st.sel[i];
                    let rad = if hot || selected { 6.5 } else { 5.5 };
                    if bv.dynamic && bv.on && !bv.spectral {
                        let ny = d.dy((bv.spec.gain_db as f32 + bv.dyn_now).clamp(-rng, rng));
                        painter.line_segment([pos, Pos2::new(pos.x, ny)], Stroke::new(3.0_f32, C_DYN));
                    }
                    let fill = if !bv.on || dim { Color32::from_rgb(24, 24, 26) } else { col };
                    if (selected || hot) && bv.on {
                        for (e, a) in [(10.0, 14u8), (6.0, 26)] {
                            painter.circle_filled(pos, rad + e, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a));
                        }
                    }
                    // flat dot: a ring of the background colour separates it from the curves
                    painter.circle_filled(pos, rad + 2.0, Color32::from_rgb(14, 15, 19));
                    painter.circle_filled(pos, rad, fill);
                    if selected {
                        painter.circle_stroke(pos, rad + 2.0, Stroke::new(2.0_f32, WHITE));
                    } else if hot {
                        painter.circle_stroke(pos, rad + 2.0, Stroke::new(1.5_f32, Color32::from_white_alpha(140)));
                    }
                    if bv.dynamic {
                        painter.circle_stroke(pos, rad + 4.5, Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(226, 176, 88, 160)));
                    }
                    if !bv.on {
                        // bypassed: a small red light
                        painter.circle_filled(pos + Vec2::new(rad, -rad), 2.5, Color32::from_rgb(220, 60, 50));
                    }
                    if bv.place != Place::Stereo {
                        painter.text(pos + Vec2::new(-rad - 2.0, -rad - 1.0), Align2::RIGHT_BOTTOM, bv.place.short(), mono(9.0), col);
                    }
                }
                if let Some((i, _)) = range_handle.as_ref() {
                    let bv = &views[*i];
                    let rr = Rect::from_center_size(Pos2::new(node_pos(*i).x, d.dy(((bv.spec.gain_db as f32) + bv.range).clamp(-rng, rng))), Vec2::new(18.0, 6.0));
                    painter.rect_filled(rr, CornerRadius::same(2), C_DYN);
                    painter.rect_stroke(rr, CornerRadius::same(2), Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Inside);
                }

                // ---------------- spectrum grab overlay: bands dimmed, frozen spectrum and its peaks on top
                if st.grab > 0 {
                    painter.rect_filled(gr, CornerRadius::ZERO, Color32::from_black_alpha(120));
                    let gcol = if st.grab == 2 { Color32::from_rgb(110, 180, 255) } else { WHITE };
                    if let Some(v) = spts {
                        let pts: Vec<Pos2> = v.iter().map(|&(f, db)| Pos2::new(d.fx(f), d.sy(db))).collect();
                        let gclip = painter.with_clip_rect(gr.shrink(1.0));
                        grad_fill(&gclip, &smooth_line(&pts, 2), gr.bottom(), gcol, 46, 4);
                        gclip.add(egui::Shape::line(smooth_line(&pts, 2), PathStroke::new(1.8_f32, gcol)));
                        for q in &peaks {
                            let qp = Pos2::new(d.fx(q.0), d.sy(q.1));
                            gclip.circle_filled(qp, 2.5, gcol);
                            let lab = if piano { note_name(q.0) } else { fmt_hz(q.0) };
                            gclip.text(qp - Vec2::new(0.0, 8.0), Align2::CENTER_BOTTOM, lab, mono(9.5), gcol);
                        }
                        if let Some((_, q)) = grab {
                            gclip.circle_stroke(q, 7.0, Stroke::new(1.5_f32, gcol));
                        }
                        // the band being dragged out of a peak stays visible
                        if let Some(dr) = st.drag.as_ref() {
                            if dr.kind == DragKind::Create {
                                if let Some(&(k, _, _, _)) = dr.bands.first() {
                                    let col = band_col(k);
                                    let pts: Vec<Pos2> = xs.iter().zip(st.cache[k].1.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(clampc(v)))).collect();
                                    gclip.add(egui::Shape::line(pts, PathStroke::new(2.0_f32, col)));
                                    painter.circle_filled(node_pos(k), 6.0, col);
                                }
                            }
                        }
                    }
                    let hint = if st.grab == 2 { "SPECTRUM GRAB (held): drag peaks to cut or boost them  ·  click the background to leave" } else { "SPECTRUM GRAB: drag a peak to cut or boost it  ·  move above the spectrum to leave" };
                    painter.text(Pos2::new(gr.center().x, gr.top() + 48.0), Align2::CENTER_CENTER, hint, mono(10.0), gcol);
                }

                // ---------------- frequency scale: hover readout, drag to zoom / pan, double-click to reset
                if pointer.is_some_and(|pp| scale_strip.contains(pp) && !in_overlay(pp)) || st.scale_drag.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                if bg.drag_started() && on_scale {
                    let o0 = ui.input(|i| i.pointer.press_origin()).unwrap_or(scale_strip.center());
                    st.scale_drag = Some((o0, st.zoom.0, st.zoom.1));
                }
                if let (Some((o0, lo0, hi0)), true) = (st.scale_drag, bg.dragged()) {
                    if let Some(pp) = pointer {
                        let (llo, lhi) = (lo0.ln(), hi0.ln());
                        let u = ((o0.x - gr.left()) / gr.width()).clamp(0.0, 1.0);
                        let anchor = llo + u * (lhi - llo);
                        let full = (F_HI / F_LO).ln();
                        // up: zoom in at the clicked frequency, sideways: pan
                        let span = ((lhi - llo) * (0.012 * (pp.y - o0.y)).exp()).clamp(2f32.ln(), full);
                        let shift = -(pp.x - o0.x) / gr.width() * span;
                        let mut lo = anchor - u * span + shift;
                        lo = lo.clamp(F_LO.ln(), F_HI.ln() - span);
                        st.zoom = (lo.exp(), (lo + span).exp());
                    }
                }
                if bg.drag_stopped() || !any_down {
                    st.scale_drag = None;
                }
                if bg.double_clicked() && on_scale {
                    st.zoom = (F_LO, F_HI);
                }
                if st.zoom != (F_LO, F_HI) {
                    painter.text(Pos2::new(gr.left() + 40.0, gr.bottom() - 26.0), Align2::LEFT_CENTER, format!("ZOOM {}–{} Hz  ·  double-click the scale to reset", fmt_hz(st.zoom.0), fmt_hz(st.zoom.1)), mono(9.0), DIM);
                }
                if let Some(pp) = pointer.filter(|pp| gr.contains(*pp) && pp.y > gr.top() + 34.0 && !in_overlay(*pp) && !st.pbox.is_some_and(|(i, _)| pbox_rect(i).contains(*pp))) {
                    let f = d.xf(pp.x);
                    if piano {
                        let m = midi_of(f).round();
                        let f0 = 440.0 * 2f32.powf((m - 0.5 - 69.0) / 12.0);
                        let f1 = 440.0 * 2f32.powf((m + 0.5 - 69.0) / 12.0);
                        let r = Rect::from_min_max(Pos2::new(d.fx(f0) + 0.3, ky), Pos2::new(d.fx(f1) - 0.3, ky + 12.0));
                        painter.rect_filled(r, CornerRadius::same(1), Color32::from_rgb(110, 180, 255));
                    }
                    let lab = if piano { format!("{} · {}", fmt_hz(f), note_name(f)) } else { format!("{} Hz", fmt_hz(f)) };
                    let w = ui.fonts(|fo| fo.layout_no_wrap(lab.clone(), mono(9.5), WHITE).size().x) + 12.0;
                    let x = pp.x.clamp(gr.left() + w * 0.5 + 2.0, gr.right() - w * 0.5 - 2.0);
                    let r = Rect::from_center_size(Pos2::new(x, gr.bottom() - 9.0), Vec2::new(w, 15.0));
                    painter.rect_filled(r, CornerRadius::same(4), C_BTN_H);
                    painter.text(r.center(), Align2::CENTER_CENTER, lab, mono(9.5), C_TXT);
                }

                // ---------------- top bar: brand | undo redo · A/B · copy | bypass · range
                let tb = Rect::from_min_size(gr.min, Vec2::new(gr.width(), 34.0));
                painter.rect_filled(tb, CornerRadius::ZERO, C_BG);
                painter.hline(tb.left()..=tb.right(), tb.bottom(), Stroke::new(1.0_f32, C_BORDER));
                painter.text(Pos2::new(tb.left() + 16.0, tb.center().y), Align2::LEFT_CENTER, "Nyashk", mono(14.0), C_TXT);
                let nw = ui.fonts(|f| f.layout_no_wrap("Nyashk".into(), mono(14.0), WHITE).size().x);
                painter.text(Pos2::new(tb.left() + 18.0 + nw, tb.center().y), Align2::LEFT_CENTER, "EQ", mono(14.0), C_DYN);
                let cx = tb.center().x;
                let tr = |x: f32, w: f32| Rect::from_min_size(Pos2::new(cx + x, tb.top() + 5.0), Vec2::new(w, 24.0));
                if ibtn(ui, tr(-132.0, 28.0), false, "Undo (Ctrl+Z / Alt+Z)", |pt, c, col| icon_arrow(pt, c, -1.0, col)).clicked() {
                    do_undo(st, setter, p);
                }
                if ibtn(ui, tr(-100.0, 28.0), false, "Redo (Ctrl+Shift+Z / Ctrl+Y)", |pt, c, col| icon_arrow(pt, c, 1.0, col)).clicked() {
                    do_redo(st, setter, p);
                }
                // A / B as one segmented control
                let seg = tr(-56.0, 56.0);
                painter.rect_filled(seg, CornerRadius::same(6), C_BTN);
                for (k, name) in [(0usize, "A"), (1, "B")] {
                    let r = Rect::from_min_size(seg.min + Vec2::new(2.0 + k as f32 * 26.0, 2.0), Vec2::new(26.0, 20.0));
                    let resp = ui.allocate_rect(r, Sense::click()).on_hover_text("A / B: two settings to compare");
                    if st.ab_cur == k {
                        painter.rect_filled(r, CornerRadius::same(5), C_TXT);
                    } else if resp.hovered() {
                        painter.rect_filled(r, CornerRadius::same(5), C_BTN_H);
                    }
                    painter.text(r.center(), Align2::CENTER_CENTER, name, mono(11.0), if st.ab_cur == k { Color32::BLACK } else { C_TXT });
                    if resp.clicked() && st.ab_cur != k {
                        st.ab[st.ab_cur] = Some(snapshot(p));
                        let other = st.ab[k].clone().unwrap_or_else(|| snapshot(p));
                        restore(setter, p, &other);
                        st.ab_cur = k;
                    }
                }
                if tbtn(ui, tr(6.0, 64.0), if st.ab_cur == 0 { "A → B" } else { "B → A" }, false, "Copy the current setting to the other slot").clicked() {
                    st.ab[1 - st.ab_cur] = Some(snapshot(p));
                }
                let br = Rect::from_min_size(Pos2::new(tb.right() - 166.0, tb.top() + 5.0), Vec2::new(86.0, 24.0));
                if tbtn(ui, br, "      Bypass", byp, "Bypass the whole plugin").clicked() {
                    set_b(setter, &p.bypass, !byp);
                }
                power_icon(&painter, Rect::from_center_size(Pos2::new(br.left() + 16.0, br.center().y), Vec2::new(16.0, 16.0)), if byp { Color32::BLACK } else { C_TXT });
                let rl = if rng > set_rng { format!("{:.0} dB ⁺", rng) } else { format!("{:.0} dB", rng) };
                let drr = Rect::from_min_size(Pos2::new(tb.right() - 72.0, tb.top() + 5.0), Vec2::new(60.0, 24.0));
                if tbtn(ui, drr, &rl, false, "Display range: click to cycle 3 / 6 / 12 / 30 dB (⁺ = widened automatically to fit the curve)").clicked() {
                    let next = match p.disp.value() {
                        DispRange::D3 => DispRange::D6,
                        DispRange::D6 => DispRange::D12,
                        DispRange::D12 => DispRange::D30,
                        DispRange::D30 => DispRange::D3,
                    };
                    set_e(setter, &p.disp, next);
                }

                // ---------------- output meter column (right): two thin bars, scale, peak readout
                let mc = rect(BASE_W - METER_W, 0.0, METER_W, BAR_Y);
                painter.rect_filled(mc, CornerRadius::ZERO, C_BG);
                painter.vline(mc.left(), mc.top()..=mc.bottom(), Stroke::new(1.0_f32, C_BORDER));
                let mt = |db: f32| mc.top() + 44.0 + (mc.height() - 60.0) * ((3.0 - db) / 63.0).clamp(0.0, 1.0);
                for c in 0..2 {
                    let x = mc.left() + 9.0 + c as f32 * 7.0;
                    let bar = Rect::from_min_max(Pos2::new(x, mt(3.0)), Pos2::new(x + 4.0, mt(-60.0)));
                    painter.rect_filled(bar, CornerRadius::same(2), C_BTN);
                    let v = st.out_pk[c].max(-60.0);
                    if v > -60.0 {
                        let fill = Rect::from_min_max(Pos2::new(x, mt(v)), Pos2::new(x + 4.0, bar.bottom()));
                        let top = if v > 0.0 { Color32::from_rgb(240, 96, 86) } else if v > -6.0 { Color32::from_rgb(236, 196, 96) } else { Color32::from_rgb(120, 214, 160) };
                        grad(&painter, fill, top, Color32::from_rgb(52, 120, 96));
                    }
                }
                for db in [0.0f32, -6.0, -12.0, -24.0, -36.0, -48.0] {
                    painter.text(Pos2::new(mc.right() - 5.0, mt(db)), Align2::RIGHT_CENTER, format!("{:.0}", db.abs()), mono(8.0), C_TXT2);
                }
                let pk = st.out_pk[0].max(st.out_pk[1]).max(-90.0);
                painter.text(Pos2::new(mc.center().x, mc.top() + 17.0), Align2::CENTER_CENTER, if pk > -89.0 { format!("{:.1}", pk) } else { "–∞".into() }, mono(9.5), if pk > 0.0 { Color32::from_rgb(240, 96, 86) } else { C_TXT2 });

                // ---------------- band panel (floats under the selected band)
                if let (Some(i), Some(pr)) = (st.focus, panel_rect) {
                    band_panel(ui, setter, p, sh, st, &views, i, pr, &painter);
                }

                // ---------------- parameter display beside the dot under the mouse
                if let Some(i) = st.hover.filter(|_| st.drag.is_none() && st.solo_drag.is_none()) {
                    st.pbox = Some((i, now));
                }
                if let Some((i, t)) = st.pbox {
                    if !views[i].used || st.drag.is_some() {
                        st.pbox = None;
                    } else {
                        let br = pbox_rect(i);
                        let np = node_pos(i);
                        let zone = br.union(Rect::from_center_size(np, Vec2::splat(26.0))).expand(4.0);
                        let keep = pointer.is_some_and(|pp| zone.contains(pp)) || st.solo_drag.is_some_and(|s| s.0 == i) || ui.memory(|m| m.any_popup_open());
                        if keep {
                            st.pbox = Some((i, now));
                        }
                        if !keep && now - t > 0.3 {
                            st.pbox = None;
                        } else {
                            param_display(ui, setter, p, sh, st, &views, i, br, &painter, piano);
                        }
                    }
                }

                // ---------------- value entry (double-click a dot)
                let just_opened = open_edit.is_some() || st.edit.as_ref().is_some_and(|e| e.first);
                if let Some(i) = open_edit {
                    let b = &p.bands[i];
                    st.edit = Some(Edit { band: i, text: [fmt_hz(b.freq.value()), format!("{:.1}", b.gain.value()), format!("{:.2}", b.q.value())], first: true });
                }
                if let Some(ed) = st.edit.as_mut() {
                    let i = ed.band;
                    if !views[i].used {
                        st.edit = None;
                    } else {
                        let br = pbox_rect(i);
                        let er = Rect::from_min_size(br.min, Vec2::new(196.0, 56.0));
                        let _ = ui.interact(er, egui::Id::new("edit_bg"), Sense::click());
                        glass(&painter, er, Some(band_col(i)));
                        let labels = ["FREQ", "GAIN", "Q"];
                        let mut commit = false;
                        for k in 0..3 {
                            let x = er.left() + 6.0 + k as f32 * 63.0;
                            painter.text(Pos2::new(x + 29.0, er.top() + 10.0), Align2::CENTER_CENTER, labels[k], mono(8.5), DIM);
                            let te = egui::TextEdit::singleline(&mut ed.text[k]).id(egui::Id::new(("eq_edit", k))).font(mono(11.0)).desired_width(56.0).horizontal_align(egui::Align::Center);
                            let r = ui.put(Rect::from_min_size(Pos2::new(x, er.top() + 20.0), Vec2::new(58.0, 26.0)), te);
                            if ed.first && k == 0 {
                                r.request_focus();
                                ed.first = false;
                            }
                            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                commit = true;
                            }
                        }
                        if commit {
                            let b = &p.bands[i];
                            if let Some(f) = crate::params::parse_hz(&ed.text[0]) {
                                set_f(setter, &b.freq, f.clamp(10.0, 30000.0));
                            }
                            if let Ok(g) = ed.text[1].trim().trim_end_matches("dB").trim().parse::<f32>() {
                                set_f(setter, &b.gain, g.clamp(-30.0, 30.0));
                            }
                            if let Ok(q) = ed.text[2].trim().parse::<f32>() {
                                set_f(setter, &b.q, q.clamp(0.025, 40.0));
                            }
                            st.edit = None;
                        } else if ui.input(|i| i.key_pressed(Key::Escape)) || (!just_opened && ui.input(|i| i.pointer.any_pressed()) && pointer.is_some_and(|pp| !er.contains(pp))) {
                            st.edit = None;
                        }
                    }
                }

                // ---------------- analyzer panel (hover the button, click to keep it)
                let ana_hovered = pointer.is_some_and(|pp| ana_btn.contains(pp) || ana_rect.is_some_and(|r| r.contains(pp)));
                if st.pop == Pop::None && pointer.is_some_and(|pp| ana_btn.contains(pp)) {
                    st.pop = Pop::AnalyzerHover;
                } else if st.pop == Pop::AnalyzerHover && !ana_hovered {
                    st.pop = Pop::None;
                }
                if st.pop == Pop::Analyzer && ui.input(|i| i.pointer.any_pressed()) && !ana_hovered {
                    st.pop = Pop::None;
                }
                if let Some(ar) = if st.pop != Pop::None { Some(Rect::from_min_size(Pos2::new(ana_btn.left() - 140.0, ana_btn.top() - 222.0), Vec2::new(440.0, 214.0))) } else { None } {
                    analyzer_panel(ui, setter, p, st, ar, &painter, fs);
                }

                // ---------------- bottom bar
                bottom_bar(ui, setter, p, sh, st, o, &painter, fs, ana_btn);
                resize_grip(ui, ctx, &size_state, full, BASE_W, BASE_H, 760.0);
            });
        },
    )
}

/// right-click menu of a band
fn band_menu(ui: &mut Ui, setter: &ParamSetter, p: &EqParams, i: usize, views: &[BandView]) {
    let b = &p.bands[i];
    ui.set_min_width(190.0);
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
            if s.for_shape(b.shape.value()) == s && ui.selectable_label(b.slope.value() == s, t(&format!("{} dB/oct", s.label()))).clicked() {
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
        if ui.button(t(if dynamic { "Make static" } else { "Make dynamic" })).clicked() {
            set_f(setter, &b.dyn_range, if dynamic { 0.0 } else if b.gain.value() > 0.0 { 6.0 } else { -6.0 });
            set_b(setter, &b.spectral, false);
            ui.close_menu();
        }
        let spec = b.spectral.value() && dynamic;
        if ui.button(t(if spec { "Spectral off" } else { "Make spectral" })).clicked() {
            if !dynamic {
                set_f(setter, &b.dyn_range, if b.gain.value() > 0.0 { 6.0 } else { -6.0 });
            }
            set_b(setter, &b.spectral, !spec);
            ui.close_menu();
        }
        if ui.button(t("Invert gain")).clicked() {
            set_f(setter, &b.gain, -b.gain.value());
            set_f(setter, &b.dyn_range, -b.dyn_range.value());
            ui.close_menu();
        }
    }
    if ui.button(t("Copy")).clicked() {
        if let Ok(mut c) = CLIP.lock() {
            *c = vec![copy_band(p, i)];
        }
        ui.close_menu();
    }
    if ui.button(t("Duplicate")).clicked() {
        if let Some(&k) = free_bands(p).first() {
            paste_band(setter, p, k, &copy_band(p, i));
            set_f(setter, &p.bands[k].freq, (b.freq.value() * 1.12).min(30000.0));
        }
        ui.close_menu();
    }
    if ui.button(t("Reset")).clicked() {
        let (f, s) = (b.freq.value(), b.shape.value());
        reset_band(setter, b);
        set_f(setter, &b.freq, f);
        set_e(setter, &b.shape, s);
        ui.close_menu();
    }
    ui.separator();
    if ui.button(t(if b.on.value() { "Bypass" } else { "Enable" })).clicked() {
        set_b(setter, &b.on, !b.on.value());
        ui.close_menu();
    }
    if ui.button(t("Delete")).clicked() {
        delete_band(setter, b);
        ui.close_menu();
    }
    let _ = views;
}

/// split a band into left / right or mid / side copies
fn split_band(setter: &ParamSetter, p: &EqParams, i: usize, ms: bool) {
    let b = &p.bands[i];
    let (a, c) = if ms { (Place::Mid, Place::Side) } else { (Place::Left, Place::Right) };
    if let Some(&k) = free_bands(p).first() {
        paste_band(setter, p, k, &copy_band(p, i));
        set_e(setter, &p.bands[k].place, c);
        set_e(setter, &b.place, a);
    }
}

fn power_icon(painter: &egui::Painter, r: Rect, col: Color32) {
    let c0 = r.center() + Vec2::new(0.0, 1.0);
    let rad = (r.height() * 0.27).min(6.0);
    let arc: Vec<Pos2> = (0..=20)
        .map(|k| {
            let a = -std::f32::consts::FRAC_PI_2 + 0.6 + (2.0 * std::f32::consts::PI - 1.2) * k as f32 / 20.0;
            c0 + Vec2::new(a.cos(), a.sin()) * rad
        })
        .collect();
    painter.add(egui::Shape::line(arc, PathStroke::new(1.4_f32, col)));
    painter.line_segment([c0 - Vec2::new(0.0, rad + 1.5), c0 - Vec2::new(0.0, rad * 0.3)], Stroke::new(1.4_f32, col));
}

/// The box beside the dot under the mouse: buttons (bypass, solo, shape, slope, menu, delete) and
/// the values, which can be dragged, wheeled or double-clicked (typed).
#[allow(clippy::too_many_arguments)]
fn param_display(ui: &mut Ui, setter: &ParamSetter, p: &EqParams, sh: &Shared, st: &mut Gui, views: &[BandView], i: usize, br: Rect, painter: &egui::Painter, piano: bool) {
    let b = &p.bands[i];
    let bv = views[i];
    let col = band_col(i);
    let _ = ui.interact(br, egui::Id::new("pbox_bg"), Sense::click_and_drag());
    glass(painter, br, Some(col));
    let bt = |k: f32| Rect::from_min_size(Pos2::new(br.left() + 5.0 + k * 25.0, br.top() + 4.0), Vec2::new(22.0, 19.0));
    // bypass
    let on = b.on.value();
    if tbtn(ui, bt(0.0), "", on, "Band on / off (Alt-click the dot)").clicked() {
        set_b(setter, &b.on, !on);
    }
    power_icon(painter, bt(0.0), if on { Color32::BLACK } else { WHITE });
    // solo: hold; drag sideways = frequency, up / down = listening level, Ctrl = Q
    let sr = bt(1.0);
    let solo_on = sh.solo.load(Ordering::Relaxed) == i as i32;
    let solo = ui.interact(sr, egui::Id::new(("pbox_solo", i)), Sense::click_and_drag()).on_hover_text("Solo: hold to hear what this band works on. Drag left / right: frequency, up / down: listening level, Ctrl + drag: Q.");
    painter.rect_filled(sr, CornerRadius::same(5), if solo_on { WHITE } else if solo.hovered() { PANEL2 } else { Color32::from_rgb(16, 16, 16) });
    painter.rect_stroke(sr, CornerRadius::same(5), Stroke::new(1.0_f32, if solo_on { WHITE } else { LINE }), StrokeKind::Inside);
    headphones(painter, sr.center(), if solo_on { Color32::BLACK } else { WHITE });
    if solo.is_pointer_button_down_on() || solo.dragged() {
        if st.solo_drag.is_none() {
            st.solo_drag = Some((i, solo.interact_pointer_pos().unwrap_or(sr.center()), b.freq.value(), Shared::get(&sh.solo_gain), b.q.value()));
            setter.begin_set_parameter(&b.freq);
            setter.begin_set_parameter(&b.q);
        }
        sh.solo.store(i as i32, Ordering::Relaxed);
    }
    if let (Some((k, start, f0, l0, q0)), Some(pp)) = (st.solo_drag, ui.input(|inp| inp.pointer.interact_pos())) {
        if k == i && ui.input(|inp| inp.pointer.primary_down()) {
            let (dx, dy) = (pp.x - start.x, pp.y - start.y);
            let ctrl = ui.input(|inp| inp.modifiers.command);
            let shp = b.shape.value();
            let cut_like = matches!(shp, Shape::LowCut | Shape::HighCut | Shape::Notch | Shape::BandPass);
            setter.set_parameter(&b.freq, (f0 * 2f32.powf(dx / 70.0)).clamp(10.0, 30000.0));
            if ctrl || cut_like {
                setter.set_parameter(&b.q, (q0 * 2f32.powf(-dy / 60.0)).clamp(0.025, 40.0));
            } else {
                sh.solo_gain.store((l0 - dy * 0.1).clamp(-24.0, 24.0).to_bits(), Ordering::Relaxed);
            }
        }
    }
    if st.solo_drag.is_some_and(|s| s.0 == i) && !ui.input(|inp| inp.pointer.primary_down()) {
        st.solo_drag = None;
        setter.end_set_parameter(&b.freq);
        setter.end_set_parameter(&b.q);
        sh.solo.store(-1, Ordering::Relaxed);
        sh.solo_gain.store(0f32.to_bits(), Ordering::Relaxed);
    }
    // shape
    let shr = bt(2.0);
    let sresp = tbtn(ui, shr, "", false, "Shape");
    shape_icon(painter, shr.shrink2(Vec2::new(3.0, 4.0)), bv.spec.shape, col);
    let sid = ui.make_persistent_id(("pbox_shape", i));
    if sresp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(sid));
    }
    egui::popup_below_widget(ui, sid, &sresp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(130.0);
        for s in Shape::ALL {
            if ui.selectable_label(bv.spec.shape == s, egui::RichText::new(s.short()).font(mono(11.5))).clicked() {
                set_e(setter, &b.shape, s);
            }
        }
    });
    // slope (cuts only, like the parameter display of the reference workflow)
    let mut nx = 3.0;
    if matches!(bv.spec.shape, Shape::LowCut | Shape::HighCut) {
        let slr = Rect::from_min_size(Pos2::new(br.left() + 5.0 + 3.0 * 25.0, br.top() + 4.0), Vec2::new(40.0, 19.0));
        let lab = bv.spec.slope.label().to_string();
        let resp = tbtn(ui, slr, &lab, false, "Slope (dB/oct)");
        let id = ui.make_persistent_id(("pbox_slope", i));
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup_below_widget(ui, id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(110.0);
            for s in Slope::ALL {
                if s.for_shape(bv.spec.shape) == s && ui.selectable_label(bv.spec.slope == s, egui::RichText::new(if s == Slope::Brick { "Brickwall".to_string() } else { format!("{} dB/oct", s.label()) }).font(mono(11.5))).clicked() {
                    set_e(setter, &b.slope, s);
                }
            }
        });
        nx = 4.75;
    }
    // band menu
    let mr = bt(nx);
    let mresp = tbtn(ui, mr, "▾", false, "Band menu");
    let mid = ui.make_persistent_id(("pbox_menu", i));
    if mresp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(mid));
    }
    egui::popup_below_widget(ui, mid, &mresp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| band_menu(ui, setter, p, i, views));
    // delete
    if tbtn(ui, Rect::from_min_size(Pos2::new(br.right() - 27.0, br.top() + 4.0), Vec2::new(22.0, 19.0)), "x", false, "Delete the band").clicked() {
        delete_band(setter, b);
        st.sel[i] = false;
        if st.focus == Some(i) {
            st.focus = None;
        }
        st.pbox = None;
        return;
    }
    // values
    let vy = br.top() + 27.0;
    let fl = if piano { note_name(bv.spec.freq as f32) } else { format!("{} Hz", fmt_hz(b.freq.value())) };
    let cells: [(Rect, String, u8); 3] = [
        (Rect::from_min_size(Pos2::new(br.left() + 3.0, vy), Vec2::new(72.0, 19.0)), fl, 0),
        (
            Rect::from_min_size(Pos2::new(br.left() + 76.0, vy), Vec2::new(62.0, 19.0)),
            if bv.spec.shape.has_gain() { format!("{:+.1} dB", b.gain.value()) } else { format!("{} dB/oct", bv.spec.slope.label()) },
            1,
        ),
        (Rect::from_min_size(Pos2::new(br.left() + 139.0, vy), Vec2::new(54.0, 19.0)), if bv.spec.shape.has_q() { format!("Q {:.2}", b.q.value()) } else { String::new() }, 2),
    ];
    for (r, txt, k) in cells {
        if txt.is_empty() {
            continue;
        }
        let (resp, dv) = value_cell(ui, r, egui::Id::new(("pbox_val", i, k)), &txt, if k == 0 { WHITE } else { MID });
        if resp.drag_started() {
            setter.begin_set_parameter(&b.freq);
            setter.begin_set_parameter(&b.gain);
            setter.begin_set_parameter(&b.q);
        }
        if dv != 0.0 {
            match k {
                0 => setter.set_parameter(&b.freq, (b.freq.value() * 2f32.powf(dv / 100.0)).clamp(10.0, 30000.0)),
                1 if bv.spec.shape.has_gain() => setter.set_parameter(&b.gain, ((b.gain.value() + dv * 0.1) * 10.0).round() / 10.0),
                _ => setter.set_parameter(&b.q, (b.q.value() * 2f32.powf(dv / 80.0)).clamp(0.025, 40.0)),
            }
        }
        if resp.drag_stopped() {
            setter.end_set_parameter(&b.freq);
            setter.end_set_parameter(&b.gain);
            setter.end_set_parameter(&b.q);
        }
        if resp.double_clicked() {
            st.edit = Some(Edit { band: i, text: [fmt_hz(b.freq.value()), format!("{:.1}", b.gain.value()), format!("{:.2}", b.q.value())], first: true });
        }
    }
}

/// The band controls panel, floating under the selected band. 8 px grid:
/// | power  shape / slope      FREQ   GAIN(+dynamics ring)   Q     placement  ×   |
/// | ◀ BAND n ▶                                                 split  »  solo      |
#[allow(clippy::too_many_arguments)]
fn band_panel(ui: &mut Ui, setter: &ParamSetter, p: &EqParams, sh: &Shared, st: &mut Gui, views: &[BandView], i: usize, pr: Rect, painter: &egui::Painter) {
    let b = &p.bands[i];
    let bv = views[i];
    let col = band_col(i);
    let has_gain = bv.spec.shape.has_gain();
    let _ = ui.interact(pr, egui::Id::new("band_panel_bg"), Sense::click_and_drag());
    glass(painter, pr, Some(col));
    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + y), Vec2::new(w, h));
    // left column
    let on = b.on.value();
    if ibtn(ui, at(16.0, 16.0, 24.0, 24.0), on, "Band on / off (Alt-click the dot)", |pt, c, k| power_icon(pt, Rect::from_center_size(c, Vec2::splat(16.0)), k)).clicked() {
        set_b(setter, &b.on, !on);
    }
    let shape_items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.short())).collect();
    let sr = at(48.0, 16.0, 120.0, 24.0);
    menu_btn(ui, setter, &b.shape, sr, "", &shape_items, "Shape");
    shape_icon(painter, Rect::from_min_size(sr.min + Vec2::new(8.0, 5.0), Vec2::new(22.0, 14.0)), bv.spec.shape, col);
    painter.text(Pos2::new(sr.left() + 36.0, sr.center().y), Align2::LEFT_CENTER, bv.spec.shape.short(), mono(10.5), C_TXT);
    if bv.spec.shape.has_slope() {
        let items: Vec<(Slope, String)> = Slope::ALL.iter().filter(|s| s.for_shape(bv.spec.shape) == **s).map(|s| (*s, if *s == Slope::Brick { "Brickwall".to_string() } else { format!("{} dB/oct", s.label()) })).collect();
        let si: Vec<(Slope, &str)> = items.iter().map(|(s, n)| (*s, n.as_str())).collect();
        let lab = if bv.spec.slope == Slope::Brick { "Brickwall".to_string() } else { format!("{} dB/oct", bv.spec.slope.label()) };
        menu_btn(ui, setter, &b.slope, at(48.0, 48.0, 120.0, 24.0), &lab, &si, "Slope");
    }
    let mut used: Vec<usize> = (0..NB).filter(|&k| views[k].used).collect();
    used.sort_by(|&a, &c| views[a].spec.freq.total_cmp(&views[c].spec.freq));
    let pos_in = used.iter().position(|&k| k == i).unwrap_or(0);
    if tbtn(ui, at(16.0, 84.0, 24.0, 22.0), "‹", false, "Previous band").clicked() {
        let k = used[(pos_in + used.len() - 1) % used.len()];
        st.focus = Some(k);
        st.sel = [false; NB];
        st.sel[k] = true;
    }
    painter.circle_filled(pr.left_top() + Vec2::new(62.0, 95.0), 4.0, col);
    painter.text(pr.left_top() + Vec2::new(72.0, 95.0), Align2::LEFT_CENTER, format!("Band {}", i + 1), mono(10.5), C_TXT);
    if tbtn(ui, at(144.0, 84.0, 24.0, 22.0), "›", false, "Next band").clicked() {
        let k = used[(pos_in + 1) % used.len()];
        st.focus = Some(k);
        st.sel = [false; NB];
        st.sel[k] = true;
    }
    // knobs, evenly spaced
    let cell = |cx: f32, r: f32| Rect::from_min_size(Pos2::new(pr.left() + cx - 40.0, pr.top() + 60.0 - r - 4.0 - 6.0), Vec2::new(80.0, 2.0 * r + 30.0));
    fknob(ui, setter, &b.freq, "FREQ", cell(222.0, 26.0), "Frequency, 10 Hz to 30 kHz (double-click the dot to type it)", false, 26.0, col);
    if has_gain {
        gain_knob(ui, setter, b, cell(312.0, 30.0), 30.0, col, bv.dyn_now, p.gain_scale.value() / 100.0);
        if bv.dynamic {
            let don = b.dyn_on.value();
            if ibtn(ui, at(266.0, 10.0, 20.0, 18.0), don, "Dynamics on / off (the range is kept)", |pt, c, k| power_icon(pt, Rect::from_center_size(c, Vec2::splat(13.0)), k)).clicked() {
                set_b(setter, &b.dyn_on, !don);
            }
            let spc = b.spectral.value();
            if tbtn(ui, at(338.0, 10.0, 20.0, 18.0), "S", spc, "Spectral dynamics").clicked() {
                set_b(setter, &b.spectral, !spc);
            }
        }
        if bv.spec.shape == Shape::Bell {
            let gq = b.gq.value();
            if tbtn(ui, at(352.0, 90.0, 28.0, 18.0), "GQ", gq, "Gain-Q interaction: the bell narrows as the gain grows").clicked() {
                set_b(setter, &b.gq, !gq);
            }
        }
    } else {
        caption(painter, pr.left_top() + Vec2::new(312.0, 60.0), Align2::CENTER_CENTER, "NO GAIN");
    }
    if bv.spec.shape.has_q() {
        fknob(ui, setter, &b.q, "Q", cell(406.0, 26.0), "Q: bandwidth / resonance (wheel over the dot)", false, 26.0, col);
    }
    // right column
    let pl_items = [(Place::Stereo, "Stereo"), (Place::Left, "Left"), (Place::Right, "Right"), (Place::Mid, "Mid"), (Place::Side, "Side")];
    menu_btn(ui, setter, &b.place, at(432.0, 16.0, 88.0, 24.0), &format!("{:?}", bv.place), &pl_items, "Stereo placement");
    let spr = at(432.0, 48.0, 40.0, 24.0);
    let sresp = ibtn(ui, spr, false, "Split into left / right or mid / side copies", icon_split);
    let sid = ui.make_persistent_id(("split", i));
    if sresp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(sid));
    }
    egui::popup_below_widget(ui, sid, &sresp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(130.0);
        if ui.button(egui::RichText::new("Split Left / Right").font(mono(11.5))).clicked() {
            split_band(setter, p, i, false);
        }
        if ui.button(egui::RichText::new("Split Mid / Side").font(mono(11.5))).clicked() {
            split_band(setter, p, i, true);
        }
    });
    if has_gain && tbtn(ui, at(480.0, 48.0, 40.0, 24.0), if st.dyn_open { "«" } else { "»" }, st.dyn_open, "Dynamics panel: threshold, attack, release, trigger, spectral").clicked() {
        st.dyn_open = !st.dyn_open;
    }
    let ears = at(432.0, 84.0, 88.0, 22.0);
    let solo_on = sh.solo.load(Ordering::Relaxed) == i as i32;
    let ear = tbtn(ui, ears, "     Solo", solo_on, "Hold to hear what this band works on (or hold the headphones next to the dot and drag)");
    headphones(painter, Pos2::new(ears.left() + 22.0, ears.center().y), if solo_on { Color32::BLACK } else { C_TXT });
    if ear.is_pointer_button_down_on() {
        sh.solo.store(i as i32, Ordering::Relaxed);
    } else if solo_on && st.solo_drag.is_none() && !ui.input(|inp| inp.pointer.primary_down()) {
        sh.solo.store(-1, Ordering::Relaxed);
    }
    // close (top right corner)
    let cr = Rect::from_min_size(Pos2::new(pr.right() - 26.0, pr.top() + 6.0), Vec2::new(20.0, 20.0));
    let crr = ui.allocate_rect(cr, Sense::click()).on_hover_text("Delete the band");
    if crr.hovered() {
        painter.rect_filled(cr, CornerRadius::same(5), C_BTN_H);
    }
    icon_close(painter, cr.center(), if crr.hovered() { C_TXT } else { C_TXT2 });
    if crr.clicked() {
        delete_band(setter, b);
        st.focus = None;
        st.sel[i] = false;
        return;
    }
    // dynamics section
    if st.dyn_open && has_gain {
        let x0 = 560.0;
        painter.vline(pr.left() + x0 - 8.0, pr.top() + 14.0..=pr.bottom() - 14.0, Stroke::new(1.0_f32, C_BORDER));
        let lvl = Shared::get(&sh.lvl_db[i]);
        let thr_now = Shared::get(&sh.thr_db[i]);
        caption(painter, pr.left_top() + Vec2::new(x0 + 16.0, 12.0), Align2::CENTER_CENTER, "THRESH");
        thr_slider(ui, setter, b, at(x0 + 6.0, 22.0, 20.0, 76.0), lvl, thr_now);
        let auto = b.thr_auto.value();
        if tbtn(ui, at(x0 + 30.0, 22.0, 22.0, 20.0), "A", auto, "Automatic threshold").clicked() {
            set_b(setter, &b.thr_auto, !auto);
        }
        let ext = b.sc_ext.value();
        if tbtn(ui, at(x0 + 30.0, 46.0, 22.0, 20.0), "E", ext, "External side chain triggers the band").clicked() {
            set_b(setter, &b.sc_ext, !ext);
        }
        let small = |cx: f32| Rect::from_min_size(Pos2::new(pr.left() + cx - 30.0, pr.top() + 22.0), Vec2::new(60.0, 60.0));
        fknob(ui, setter, &b.attack, "ATTACK", small(x0 + 88.0), "Attack (50 % = auto)", true, 18.0, C_DYN);
        fknob(ui, setter, &b.release, "RELEASE", small(x0 + 140.0), "Release (50 % = auto)", true, 18.0, C_DYN);
        let tx = x0 + 176.0;
        let free = b.sc_free.value();
        if tbtn(ui, at(tx, 16.0, 84.0, 22.0), if free { "Trigger: Free" } else { "Trigger: Band" }, free, "Trigger: the band's own range, or your own low / high cut").clicked() {
            set_b(setter, &b.sc_free, !free);
        }
        let aud = tbtn(ui, at(tx, 44.0, 84.0, 22.0), "Audition", false, "Hold to hear the trigger signal");
        if aud.is_pointer_button_down_on() {
            sh.solo.store(i as i32, Ordering::Relaxed);
        }
        let spc = b.spectral.value();
        if tbtn(ui, at(tx, 72.0, 84.0, 22.0), "Spectral", spc, "Spectral dynamics").clicked() {
            set_b(setter, &b.spectral, !spc);
        }
        if tbtn(ui, at(tx, 98.0, 84.0, 16.0), "Clear", false, "Clear the dynamics").clicked() {
            set_f(setter, &b.dyn_range, 0.0);
            set_b(setter, &b.spectral, false);
        }
        let mut kx = tx + 126.0;
        if spc {
            fknob(ui, setter, &b.density, "DENSITY", small(kx), "Spectral density", false, 17.0, C_DYN);
            let tl = b.sp_tilt.value();
            if tbtn(ui, at(kx - 22.0, 92.0, 44.0, 18.0), "Tilt", tl, "Spectral tilt: +3 dB/oct on the trigger").clicked() {
                set_b(setter, &b.sp_tilt, !tl);
            }
            kx += 56.0;
        }
        if free {
            fknob(ui, setter, &b.sc_lo, "LO CUT", small(kx), "Trigger low cut", false, 17.0, C_DYN);
            fknob(ui, setter, &b.sc_hi, "HI CUT", small(kx + 54.0), "Trigger high cut", false, 17.0, C_DYN);
        }
    }
    if sh.dyn_blocked.load(Ordering::Relaxed) && bv.dynamic {
        painter.text(Pos2::new(pr.center().x, pr.top() - 10.0), Align2::CENTER_CENTER, "dynamics off: Linear Phase above High resolution", mono(9.5), C_DYN);
    }
}

/// analyzer settings (opens on hover over the button, stays when clicked)
fn analyzer_panel(ui: &mut Ui, setter: &ParamSetter, p: &EqParams, st: &mut Gui, ar: Rect, painter: &egui::Painter, _fs: f32) {
    let _ = ui.interact(ar, egui::Id::new("ana_pop_bg"), Sense::click_and_drag());
    glass(painter, ar, None);
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
    let fr = tbtn(ui, at(172.0, 22.0, 66.0, 22.0), "FREEZE", fz || st.freeze_hold, "Freeze: the spectrum stops falling and keeps its maximum. Click to switch, hold to freeze only while held.");
    let now = ui.input(|i| i.time);
    if fr.is_pointer_button_down_on() {
        let t0 = *st.freeze_t.get_or_insert(now);
        st.freeze_hold = now - t0 > 0.3;
    } else {
        if fr.clicked() && !st.freeze_hold {
            set_b(setter, &p.an_freeze, !fz);
        }
        st.freeze_t = None;
        st.freeze_hold = false;
    }
    let gb = p.an_grab.value();
    if tbtn(ui, at(242.0, 22.0, 66.0, 22.0), "GRAB", gb, "Spectrum grab: rest the mouse in the spectrum for a second (or click and hold for a permanent grab), then drag a peak to make a band there.").clicked() {
        set_b(setter, &p.an_grab, !gb);
    }
    let cl = p.an_coll.value();
    if tbtn(ui, at(312.0, 22.0, 116.0, 22.0), "COLLISIONS", cl, "Red where this track and the side chain are both loud.").clicked() {
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
    painter.hline(ar.left() + 10.0..=ar.right() - 10.0, ar.top() + 164.0, Stroke::new(1.0_f32, LINE));
    lab(12.0, 186.0, "EQ MATCH");
    if tbtn(ui, at(98.0, 176.0, 110.0, 22.0), if st.learn { "LISTENING…" } else { "LEARN" }, st.learn, "Play your track with the reference on the side-chain input; click to start / stop learning both spectra.").clicked() {
        st.learn = !st.learn;
        st.pop = Pop::Analyzer;
    }
    let ready = st.m_cnt > 30;
    if tbtn(ui, at(212.0, 176.0, 80.0, 22.0), "MATCH", false, "Add bands that make this track follow the reference.").clicked() && ready {
        st.learn = false;
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
        for j in 0..FIT_N {
            let (lo, hi) = (j.saturating_sub(2), (j + 2).min(FIT_N - 1));
            r[j] = raw[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
        }
        let free = free_bands(p);
        for ((f, g, q), &k) in fit_bells(&mut r, 0, FIT_N - 1, free.len().min(8)).iter().zip(free.iter()) {
            put_bell(setter, &p.bands[k], *f, *g, *q);
        }
    }
    if tbtn(ui, at(296.0, 176.0, 60.0, 22.0), "RESET", false, "Forget what was learned.").clicked() {
        st.m_pre.iter_mut().for_each(|v| *v = 0.0);
        st.m_sc.iter_mut().for_each(|v| *v = 0.0);
        st.m_cnt = 0;
    }
    painter.text(Pos2::new(ar.left() + 364.0, ar.top() + 187.0), Align2::LEFT_CENTER, format!("{:.0} s", st.m_cnt as f32 * 0.03), mono(9.5), if ready { WHITE } else { DIM });
}

/// bottom bar item: dim caption + bright value, the whole thing is the button
fn bar_item(ui: &mut Ui, painter: &egui::Painter, x: f32, y: f32, cap: &str, val: &str, on: bool, tip: &str) -> (egui::Response, f32) {
    let cw = if cap.is_empty() { 0.0 } else { ui.fonts(|f| f.layout_no_wrap(cap.to_string(), mono(8.5), WHITE).size().x) + 6.0 };
    let vw = ui.fonts(|f| f.layout_no_wrap(val.to_string(), mono(11.0), WHITE).size().x);
    let r = Rect::from_min_size(Pos2::new(x, y + 4.0), Vec2::new(cw + vw + 16.0, 24.0));
    let resp = ui.allocate_rect(r, Sense::click_and_drag());
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    if resp.hovered() || resp.dragged() {
        painter.rect_filled(r, CornerRadius::same(6), C_BTN);
    }
    if cw > 0.0 {
        painter.text(Pos2::new(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, cap, mono(8.5), C_TXT2);
    }
    painter.text(Pos2::new(r.left() + 8.0 + cw, r.center().y), Align2::LEFT_CENTER, val, mono(11.0), if on { C_TXT } else { C_TXT2 });
    (resp, r.right())
}

/// bottom bar: sketch · piano | processing · resolution · latency | character | analyzer |
/// phase · auto gain · scale · output · pan
#[allow(clippy::too_many_arguments)]
fn bottom_bar(ui: &mut Ui, setter: &ParamSetter, p: &EqParams, sh: &Shared, st: &mut Gui, o: Pos2, painter: &egui::Painter, fs: f32, ana_btn: Rect) {
    let y = o.y + BAR_Y;
    let bar = Rect::from_min_size(Pos2::new(o.x, y), Vec2::new(BASE_W, BASE_H - BAR_Y));
    painter.rect_filled(bar, CornerRadius::ZERO, C_BG);
    painter.hline(bar.left()..=bar.right(), bar.top(), Stroke::new(1.0_f32, C_BORDER));
    let sep = |x: f32| painter.vline(x + 6.0, y + 10.0..=y + 22.0, Stroke::new(1.0_f32, C_BORDER));
    let piano = p.piano.value();
    let mut x = o.x + 8.0;
    let (r, nx) = bar_item(ui, painter, x, y, "", "Sketch", st.sketch_on, "EQ Sketch: draw a curve, bands are made to follow it");
    if r.clicked() {
        st.sketch_on = !st.sketch_on;
        st.sketch.clear();
    }
    x = nx + 2.0;
    let (r, nx) = bar_item(ui, painter, x, y, "", "Piano", piano, "Piano display: notes on the frequency axis, frequencies snap to notes");
    if r.clicked() {
        set_b(setter, &p.piano, !piano);
    }
    sep(nx);
    x = nx + 14.0;
    let mode = p.pmode.value();
    let res_name = |r: Res| match r {
        Res::Low => "Low",
        Res::Medium => "Medium",
        Res::High => "High",
        Res::VeryHigh => "Very High",
        Res::Max => "Maximum",
    };
    let warn = sh.dyn_blocked.load(Ordering::Relaxed);
    let ml = format!("{}{} ▾", match mode {
        PMode::Zero => "Zero Latency",
        PMode::Natural => "Natural Phase",
        PMode::Linear => "Linear Phase",
    }, if warn { " ⚠" } else { "" });
    let (mresp, nx) = bar_item(ui, painter, x, y, "PHASE", &ml, true, "Processing mode. Zero Latency: analog-matched, no delay. Natural Phase: analog magnitude and phase, ~8 ms. Linear Phase: no phase shift, latency from the resolution.");
    let pid = ui.make_persistent_id("pmode_pop");
    if mresp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(pid));
    }
    egui::popup_above_or_below_widget(ui, pid, &mresp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(200.0);
        for (m, name) in [(PMode::Zero, "Zero Latency"), (PMode::Natural, "Natural Phase"), (PMode::Linear, "Linear Phase")] {
            if ui.selectable_label(mode == m, egui::RichText::new(name).font(mono(11.5))).clicked() {
                set_e(setter, &p.pmode, m);
            }
        }
    });
    x = nx + 2.0;
    if mode == PMode::Linear {
        let (rresp, nx) = bar_item(ui, painter, x, y, "RES", &format!("{} ▾", res_name(p.res.value())), true, "Linear phase resolution (and its latency). Dynamic bands work up to High.");
        let rid = ui.make_persistent_id("res_pop");
        if rresp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(rid));
        }
        egui::popup_above_or_below_widget(ui, rid, &rresp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(170.0);
            for r in [Res::Low, Res::Medium, Res::High, Res::VeryHigh, Res::Max] {
                let txt = format!("{:<10}{:>5.0} ms", res_name(r), (r.n48() as f32 / 2.0 + 512.0) / 48000.0 * 1000.0);
                if ui.selectable_label(p.res.value() == r, egui::RichText::new(txt).font(mono(11.5))).clicked() {
                    set_e(setter, &p.res, r);
                }
            }
        });
        x = nx + 2.0;
    }
    let lat = sh.latency.load(Ordering::Relaxed);
    painter.text(Pos2::new(x + 6.0, y + 16.0), Align2::LEFT_CENTER, format!("{:.1} ms", lat as f32 / fs * 1000.0), mono(9.5), C_TXT2);
    x += 58.0;
    sep(x);
    x += 14.0;
    let ch = p.character.value();
    let chl = match ch {
        Character::Clean => "Clean ▾",
        Character::Subtle => "Subtle ▾",
        Character::Warm => "Warm ▾",
    };
    let (cresp, nx) = bar_item(ui, painter, x, y, "CHARACTER", chl, true, "Character: Clean, Subtle or Warm (analog-style saturation)");
    let cid = ui.make_persistent_id("char_pop");
    if cresp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(cid));
    }
    egui::popup_above_or_below_widget(ui, cid, &cresp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(110.0);
        for (c, n) in [(Character::Clean, "Clean"), (Character::Subtle, "Subtle"), (Character::Warm, "Warm")] {
            if ui.selectable_label(ch == c, egui::RichText::new(n).font(mono(11.5))).clicked() {
                set_e(setter, &p.character, c);
            }
        }
    });
    sep(nx);
    let _ = x;
    // analyzer (fixed slot: the pop-up is anchored to it)
    let mut v = vec![];
    if p.an_pre.value() {
        v.push("Pre");
    }
    if p.an_post.value() {
        v.push("Post");
    }
    if p.an_sc.value() {
        v.push("SC");
    }
    let alab = format!("{}{}", if v.is_empty() { "Off".to_string() } else { v.join(" + ") }, if p.an_freeze.value() || st.freeze_hold { " · frozen" } else { "" });
    let (ar, _) = bar_item(ui, painter, ana_btn.left(), y, "ANALYZER", &alab, true, "Analyzer (hover for settings, click to keep them open)");
    if ar.clicked() {
        st.pop = if st.pop == Pop::Analyzer { Pop::None } else { Pop::Analyzer };
    }
    if p.an_freeze.value() || st.freeze_hold || st.grab > 0 {
        // frozen: a blue line on top of the analyzer button
        painter.hline(ar.rect.left() + 6.0..=ar.rect.right() - 6.0, ar.rect.top() + 1.0, Stroke::new(2.0_f32, Color32::from_rgb(110, 180, 255)));
    }
    // output (right aligned)
    let drag_num = |ui: &mut Ui, x: f32, cap: &str, prm: &FloatParam, txt: String, tip: &str, step: f32| -> f32 {
        let (resp, nx) = bar_item(ui, painter, x, y, cap, &txt, true, tip);
        if resp.drag_started() {
            setter.begin_set_parameter(prm);
        }
        if resp.dragged() {
            let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
            setter.set_parameter(prm, prm.value() - resp.drag_delta().y * step * fine);
        }
        if resp.drag_stopped() {
            setter.end_set_parameter(prm);
        }
        if resp.double_clicked() {
            set_f(setter, prm, prm.default_plain_value());
        }
        if resp.hovered() {
            let w = ui.input(|i| i.smooth_scroll_delta.y);
            if w != 0.0 {
                set_f(setter, prm, prm.value() + w / 30.0 * step * 4.0);
            }
        }
        nx
    };
    let mut x = o.x + BASE_W - 340.0;
    sep(x - 14.0);
    let ph = p.phase.value();
    let (r, nx) = bar_item(ui, painter, x, y, "", "Ø", ph, "Phase invert");
    if r.clicked() {
        set_b(setter, &p.phase, !ph);
    }
    x = nx;
    let ag = p.auto_gain.value();
    let (r, nx) = bar_item(ui, painter, x, y, "AUTO", if ag { "On" } else { "Off" }, ag, "Auto gain: keeps the level about the same whatever the curve does");
    if r.clicked() {
        set_b(setter, &p.auto_gain, !ag);
    }
    x = nx;
    x = drag_num(ui, x, "SCALE", &p.gain_scale, format!("{:.0}%", p.gain_scale.value()), "Gain scale: drag up / down, double-click: 100 %", 1.0);
    x = drag_num(ui, x, "OUT", &p.out, format!("{:+.1} dB", p.out.value()), "Output gain: drag up / down, double-click: 0 dB", 0.1);
    let pan = p.pan.value();
    let _ = drag_num(ui, x, "PAN", &p.pan, if pan.abs() < 0.5 { "C".into() } else if pan < 0.0 { format!("L{:.0}", -pan) } else { format!("R{:.0}", pan) }, "Output pan: drag up / down, double-click: centre", 1.0);
}
