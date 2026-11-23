//! NyashkEQ 2 GUI: a big interactive display (double-click to add a band, drag, wheel = Q,
//! Alt + wheel = dynamic range, right-click menu, rubber-band selection), a floating band panel
//! with the dynamics section, and the bottom bar (processing mode, character, analyzer, output).
use nih_plug::prelude::*;
use nih_plug_egui::{
    create_egui_editor,
    egui::{self, epaint::PathStroke, Align2, Color32, CornerRadius, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2},
};
use nyashk_common::analyzer::{draw_spectrum, smooth_line, Spectrum};
use nyashk_common::ui::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::conv::{Place, NB};
use crate::filters::{analog, Shape, Slope, Spec};
use crate::params::{fmt_hz, BandParams, Character, DispRange, EqParams, PMode, Res};
use crate::Shared;

pub const BASE_W: f32 = 1080.0;
pub const BASE_H: f32 = 700.0;
const F_LO: f32 = 10.0;
const F_HI: f32 = 30000.0;
const NPTS: usize = 360;

fn band_col(i: usize) -> Color32 {
    let h = (0.02 + i as f32 * 0.161) % 1.0;
    Color32::from(egui::ecolor::Hsva::new(h, 0.55, 0.92, 1.0))
}

#[derive(Clone, Copy)]
struct Disp {
    g: Rect,
    range: f32,
}

impl Disp {
    fn fx(&self, f: f32) -> f32 {
        self.g.left() + self.g.width() * (f.max(1.0) / F_LO).ln() / (F_HI / F_LO).ln()
    }
    fn xf(&self, x: f32) -> f32 {
        F_LO * (F_HI / F_LO).powf(((x - self.g.left()) / self.g.width()).clamp(0.0, 1.0))
    }
    fn dy(&self, d: f32) -> f32 {
        self.g.center().y - self.g.height() * 0.46 * (d / self.range)
    }
    fn yd(&self, y: f32) -> f32 {
        (self.g.center().y - y) / (self.g.height() * 0.46) * self.range
    }
    /// spectrum level (dBFS) to y: -90..+6 over the full height
    fn sy(&self, db: f32) -> f32 {
        self.g.bottom() - self.g.height() * ((db + 90.0) / 96.0).clamp(0.0, 1.05)
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
}

struct Gui {
    pre: Spectrum,
    post: Spectrum,
    sc: Spectrum,
    out_pk: [f32; 2],
    sel: [bool; NB],
    focus: Option<usize>,
    hover: Option<usize>,
    drag: Option<Drag>,
    rubber: Option<Pos2>,
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
    moved: bool,
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
    painter.add(egui::Shape::line(pts, PathStroke::new(1.6, col)));
    if s == Shape::AllPass {
        painter.circle_stroke(Pos2::new(l + w * 0.5, m), h * 0.25, Stroke::new(1.4, col));
    }
}

/// small text button
fn tbtn(ui: &mut Ui, r: Rect, text: &str, on: bool, tip: &str) -> egui::Response {
    let resp = ui.allocate_rect(r, Sense::click()).on_hover_text(tip);
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect_filled(r, CornerRadius::same(5), if on { WHITE } else if hov { PANEL2 } else { Color32::from_rgb(16, 16, 16) });
    p.rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, if on { WHITE } else if hov { DIM } else { LINE }), StrokeKind::Inside);
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
    p.hline(r.left()..=r.right(), ty, Stroke::new(2.0, C_DYN));
    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, LINE), StrokeKind::Inside);
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
            pre: Spectrum::new(8192),
            post: Spectrum::new(8192),
            sc: Spectrum::new(8192),
            out_pk: [-90.0; 2],
            sel: [false; NB],
            focus: None,
            hover: None,
            drag: None,
            rubber: None,
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
            if p.an_pre.value() {
                st.pre.update(&sh.tap_pre);
            }
            if p.an_post.value() {
                st.post.update(&sh.tap_post);
            }
            if p.an_sc.value() {
                st.sc.update(&sh.tap_sc);
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
                // A / B / copy
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
                let can_undo = st.undo.len() > 1;
                if tbtn(ui, rect(hx - 54.0, 22.0, 26.0, 24.0), "↶", false, "Undo").clicked() && can_undo {
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
                let gr = rect(14.0, 68.0, BASE_W - 28.0, 400.0);
                let d = Disp { g: gr.shrink2(Vec2::new(0.0, 0.0)), range: p.disp.value().db() };
                let painter = ui.painter().clone();
                painter.rect_filled(gr, CornerRadius::same(6), Color32::from_rgb(8, 8, 8));
                let clip = painter.with_clip_rect(gr.shrink(1.0));
                // grid
                let fgrid = [20.0f32, 30.0, 40.0, 50.0, 60.0, 80.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 800.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 8000.0, 10000.0, 20000.0];
                for f in fgrid {
                    let major = matches!(f as i32, 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 | 10000 | 20000);
                    clip.vline(d.fx(f), gr.top()..=gr.bottom(), Stroke::new(1.0, Color32::from_white_alpha(if major { 14 } else { 6 })));
                }
                let rng = d.range;
                let step = match p.disp.value() {
                    DispRange::D3 => 1.0,
                    DispRange::D6 => 2.0,
                    DispRange::D12 => 3.0,
                    DispRange::D30 => 6.0,
                };
                let mut v = -rng.floor();
                while v <= rng + 0.01 {
                    let y = d.dy(v);
                    clip.hline(gr.left()..=gr.right(), y, Stroke::new(1.0, Color32::from_white_alpha(if v == 0.0 { 30 } else { 9 })));
                    if v.abs() < rng - 0.01 || v == 0.0 {
                        clip.text(Pos2::new(gr.right() - 6.0, y - 1.0), Align2::RIGHT_BOTTOM, if v == 0.0 { "0".into() } else { format!("{:+.0}", v) }, mono(9.0), if v == 0.0 { MID } else { DIM });
                    }
                    v += step;
                }
                // analyzer (tilted +4.5 dB/oct around 1 kHz)
                let top_f = F_HI.min(fs * 0.5);
                if p.an_post.value() {
                    let pts = st.post.points(fs, 20.0, top_f, 300, 4.5, |f| d.fx(f), |db| d.sy(db));
                    draw_spectrum(&clip, gr, &pts, Color32::from_rgb(26, 30, 36), Color32::from_rgb(70, 80, 96));
                }
                if p.an_pre.value() {
                    let pts = st.pre.points(fs, 20.0, top_f, 300, 4.5, |f| d.fx(f), |db| d.sy(db));
                    clip.add(egui::Shape::line(smooth_line(&pts, 3), PathStroke::new(1.0, Color32::from_rgb(64, 64, 64))));
                }
                if p.an_sc.value() {
                    let pts = st.sc.points(fs, 20.0, top_f, 300, 4.5, |f| d.fx(f), |db| d.sy(db));
                    clip.add(egui::Shape::line(smooth_line(&pts, 3), PathStroke::new(1.0, Color32::from_rgba_unmultiplied(226, 176, 88, 140))));
                }

                for f in [20.0f32, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
                    let lab = if f >= 1000.0 { format!("{}k", (f / 1000.0) as i32) } else { format!("{}", f as i32) };
                    clip.text(Pos2::new(d.fx(f), gr.bottom() - 9.0), Align2::CENTER_CENTER, lab, mono(9.5), MID);
                }
                // band data
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
                // per band: static curves (faint), focused / hovered band filled
                let zero = d.dy(0.0);
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used {
                        continue;
                    }
                    let col = band_col(i);
                    let hot = st.focus == Some(i) || st.hover == Some(i) || st.sel[i];
                    let pts: Vec<Pos2> = xs.iter().zip(st.cache[i].1.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(v.clamp(-rng * 1.2, rng * 1.2)))).collect();
                    let alpha = if !bv.on { 40 } else if hot { 200 } else { 70 };
                    if hot && bv.on {
                        fill_under(&clip, &pts, zero, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 34));
                    }
                    clip.add(egui::Shape::line(pts, PathStroke::new(if hot { 1.6 } else { 1.0 }, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha))));
                    // dynamic range area: from the static curve to the curve at gain + range
                    if bv.dynamic && bv.on && hot {
                        let mut s2 = bv.spec;
                        s2.gain_db = (s2.gain_db + bv.range as f64).clamp(-30.0, 30.0);
                        let mut v2 = Vec::with_capacity(NPTS + 1);
                        resp(&s2, &d, &mut v2);
                        let top: Vec<Pos2> = xs.iter().zip(v2.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(v.clamp(-rng * 1.2, rng * 1.2)))).collect();
                        clip.add(egui::Shape::dashed_line(&top, Stroke::new(1.0, Color32::from_rgba_unmultiplied(226, 176, 88, 150)), 4.0, 3.0));
                    }
                }
                // total curves (live, dynamics included): left / mid / stereo and right / side
                let mut tot_a = vec![0.0f32; NPTS + 1];
                let mut tot_b = vec![0.0f32; NPTS + 1];
                let mut split = false;
                let mut live = Vec::with_capacity(NPTS + 1);
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used || !bv.on {
                        continue;
                    }
                    let src: &Vec<f32> = if bv.dynamic && bv.dyn_now.abs() > 0.02 {
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
                let line_of = |t: &[f32]| -> Vec<Pos2> { xs.iter().zip(t.iter()).map(|(&x, &v)| Pos2::new(x, d.dy(v.clamp(-rng * 1.3, rng * 1.3)))).collect() };
                if split {
                    let has_ms = views.iter().any(|v| v.used && v.on && matches!(v.place, Place::Mid | Place::Side));
                    clip.add(egui::Shape::line(line_of(&tot_b), PathStroke::new(1.8, Color32::from_rgb(120, 200, 230))));
                    clip.text(Pos2::new(gr.left() + 10.0, gr.top() + 22.0), Align2::LEFT_CENTER, if has_ms { "— MID / LEFT" } else { "— LEFT" }, mono(9.5), WHITE);
                    clip.text(Pos2::new(gr.left() + 10.0, gr.top() + 36.0), Align2::LEFT_CENTER, if has_ms { "— SIDE / RIGHT" } else { "— RIGHT" }, mono(9.5), Color32::from_rgb(120, 200, 230));
                }
                let main = line_of(&tot_a);
                if !split {
                    fill_under(&clip, &main, zero, Color32::from_white_alpha(10));
                }
                clip.add(egui::Shape::line(main, PathStroke::new(2.2, if byp { DIM } else { WHITE })));

                // ---------------- interaction
                let pointer = ui.input(|i| i.pointer.hover_pos());
                let mods = ui.input(|i| i.modifiers);
                // background (created first: nodes and the panel sit on top)
                let bg = ui.interact(gr, egui::Id::new("eq_bg"), Sense::click_and_drag());
                // node positions
                let node_pos = |i: usize| -> Pos2 {
                    let bv = &views[i];
                    let y = if bv.spec.shape.has_gain() { bv.spec.gain_db as f32 } else { 0.0 };
                    Pos2::new(d.fx(bv.spec.freq as f32).clamp(gr.left() + 6.0, gr.right() - 6.0), d.dy(y.clamp(-rng, rng)).clamp(gr.top() + 6.0, gr.bottom() - 6.0))
                };
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
                // dynamic range handles of the focused band
                let mut range_handle: Option<(usize, egui::Response)> = None;
                if let Some(i) = st.focus {
                    let bv = &views[i];
                    if bv.used && bv.dynamic {
                        let y = d.dy(((bv.spec.gain_db as f32) + bv.range).clamp(-rng, rng));
                        let x = node_pos(i).x;
                        let r = ui.interact(Rect::from_center_size(Pos2::new(x, y), Vec2::new(26.0, 12.0)), egui::Id::new(("eqrange", i)), Sense::drag());
                        range_handle = Some((i, r));
                    }
                }
                st.hover = new_hover;

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
                        st.drag = Some(Drag { start: r.interact_pointer_pos().unwrap_or(node_pos(i)), bands, moved: false });
                    }
                    if r.dragged() {
                        if let (Some(dr), Some(pp)) = (st.drag.as_mut(), r.interact_pointer_pos()) {
                            dr.moved = true;
                            let fine = if mods.shift { 0.15 } else { 1.0 };
                            let dx = (pp.x - dr.start.x) * fine;
                            let dyv = (pp.y - dr.start.y) * fine;
                            let ratio = d.xf(d.g.left() + dx + d.g.width() * 0.5) / d.xf(d.g.left() + d.g.width() * 0.5);
                            let dg = -dyv / (d.g.height() * 0.46) * rng;
                            for (k, f0, g0) in &dr.bands {
                                let bk = &p.bands[*k];
                                setter.set_parameter(&bk.freq, (f0 * ratio).clamp(10.0, 30000.0));
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
                    // right-click menu
                    r.context_menu(|ui| {
                        ui.set_min_width(170.0);
                        ui.label(egui::RichText::new(format!("BAND {}", i + 1)).font(mono(11.0)).color(band_col(i)));
                        ui.menu_button(egui::RichText::new("Shape").font(mono(11.5)), |ui| {
                            for s in Shape::ALL {
                                if ui.selectable_label(b.shape.value() == s, egui::RichText::new(s.short()).font(mono(11.5))).clicked() {
                                    set_e(setter, &b.shape, s);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button(egui::RichText::new("Slope").font(mono(11.5)), |ui| {
                            for s in Slope::ALL {
                                if ui.selectable_label(b.slope.value() == s, egui::RichText::new(format!("{} dB/oct", s.label())).font(mono(11.5))).clicked() {
                                    set_e(setter, &b.slope, s);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button(egui::RichText::new("Stereo placement").font(mono(11.5)), |ui| {
                            for pl in [Place::Stereo, Place::Left, Place::Right, Place::Mid, Place::Side] {
                                if ui.selectable_label(b.place.value() == pl, egui::RichText::new(format!("{pl:?}")).font(mono(11.5))).clicked() {
                                    set_e(setter, &b.place, pl);
                                    ui.close_menu();
                                }
                            }
                        });
                        if b.shape.value().has_gain() {
                            let dynamic = b.dyn_range.value().abs() > 0.01;
                            if ui.button(egui::RichText::new(if dynamic { "Clear dynamics" } else { "Make dynamic" }).font(mono(11.5))).clicked() {
                                set_f(setter, &b.dyn_range, if dynamic { 0.0 } else if b.gain.value() > 0.0 { 6.0 } else { -6.0 });
                                ui.close_menu();
                            }
                        }
                        if ui.button(egui::RichText::new(if b.on.value() { "Disable" } else { "Enable" }).font(mono(11.5))).clicked() {
                            set_b(setter, &b.on, !b.on.value());
                            ui.close_menu();
                        }
                        if ui.button(egui::RichText::new("Delete").font(mono(11.5))).clicked() {
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
                // background: double-click adds a band, drag selects, click deselects
                if bg.double_clicked() {
                    if let Some(pp) = bg.interact_pointer_pos() {
                        if let Some(i) = create_band(setter, p, d.xf(pp.x), d.yd(pp.y).clamp(-30.0, 30.0), mods.alt) {
                            st.sel = [false; NB];
                            st.sel[i] = true;
                            st.focus = Some(i);
                        }
                    }
                } else if bg.clicked() {
                    st.sel = [false; NB];
                    st.focus = None;
                }
                if bg.drag_started() {
                    st.rubber = bg.interact_pointer_pos();
                }
                if let (Some(a), true) = (st.rubber, bg.dragged()) {
                    if let Some(b) = pointer {
                        let rr = Rect::from_two_pos(a, b);
                        clip.rect_filled(rr, CornerRadius::same(2), Color32::from_white_alpha(10));
                        clip.rect_stroke(rr, CornerRadius::same(2), Stroke::new(1.0, DIM), StrokeKind::Inside);
                        for i in 0..NB {
                            if views[i].used {
                                st.sel[i] = rr.contains(node_pos(i)) || (mods.shift && st.sel[i]);
                            }
                        }
                    }
                }
                if bg.drag_stopped() {
                    st.rubber = None;
                    st.focus = (0..NB).find(|&i| st.sel[i]);
                }
                // keyboard: delete removes the selected bands
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
                if bg.hovered() && st.hover.is_none() && !views.iter().any(|v| v.used) {
                    clip.text(gr.center(), Align2::CENTER_CENTER, "double-click to add a band   ·   Alt + double-click: dynamic band", mono(11.0), DIM);
                }

                // draw nodes
                for i in 0..NB {
                    let bv = &views[i];
                    if !bv.used {
                        continue;
                    }
                    let pos = node_pos(i);
                    let col = band_col(i);
                    let hot = st.hover == Some(i);
                    let selected = st.sel[i];
                    let rad = if hot || selected { 7.5 } else { 6.0 };
                    if bv.dynamic && bv.on {
                        // live dynamic gain: a bar from the static gain to where it is now
                        let ny = d.dy((bv.spec.gain_db as f32 + bv.dyn_now).clamp(-rng, rng));
                        painter.line_segment([Pos2::new(pos.x, pos.y), Pos2::new(pos.x, ny)], Stroke::new(3.0, C_DYN));
                        painter.circle_filled(Pos2::new(pos.x, ny), 2.5, C_DYN);
                    }
                    painter.circle_filled(pos, rad, if bv.on { col } else { Color32::from_rgb(20, 20, 20) });
                    painter.circle_stroke(pos, rad, Stroke::new(if selected { 2.0 } else { 1.2 }, if selected { WHITE } else { col }));
                    let tag = format!("{}", i + 1);
                    painter.text(pos, Align2::CENTER_CENTER, &tag, mono(if tag.len() > 1 { 8.0 } else { 9.5 }), if bv.on { Color32::BLACK } else { col });
                    if bv.place != Place::Stereo {
                        painter.text(pos + Vec2::new(rad + 2.0, -rad - 1.0), Align2::LEFT_BOTTOM, bv.place.short(), mono(9.0), col);
                    }
                    if hot {
                        let txt = if bv.spec.shape.has_gain() {
                            format!("{}  {} Hz  {:+.1} dB  Q {:.2}", bv.spec.shape.short(), fmt_hz(bv.spec.freq as f32), bv.spec.gain_db, bv.spec.q)
                        } else {
                            format!("{}  {} Hz  Q {:.2}  {}", bv.spec.shape.short(), fmt_hz(bv.spec.freq as f32), bv.spec.q, bv.spec.slope.label())
                        };
                        let gal = painter.layout_no_wrap(txt, mono(10.0), WHITE);
                        let sz = gal.size();
                        let x = (pos.x - sz.x * 0.5).clamp(gr.left() + 4.0, gr.right() - 4.0 - sz.x);
                        let y = if pos.y - 30.0 > gr.top() { pos.y - 16.0 - sz.y } else { pos.y + 14.0 };
                        painter.rect_filled(Rect::from_min_size(Pos2::new(x, y), sz).expand2(Vec2::new(4.0, 2.0)), 3.0, Color32::from_black_alpha(200));
                        painter.galley(Pos2::new(x, y), gal, WHITE);
                    }
                }
                if let Some((i, _)) = range_handle.as_ref() {
                    let bv = &views[*i];
                    let y = d.dy(((bv.spec.gain_db as f32) + bv.range).clamp(-rng, rng));
                    let x = node_pos(*i).x;
                    let rr = Rect::from_center_size(Pos2::new(x, y), Vec2::new(18.0, 6.0));
                    painter.rect_filled(rr, CornerRadius::same(2), if bv.dynamic { C_DYN } else { Color32::from_rgba_unmultiplied(226, 176, 88, 60) });
                    painter.rect_stroke(rr, CornerRadius::same(2), Stroke::new(1.0, Color32::BLACK), StrokeKind::Inside);
                }

                // display range button (top right of the display)
                let dr = Rect::from_min_size(Pos2::new(gr.right() - 64.0, gr.top() + 8.0), Vec2::new(56.0, 20.0));
                if tbtn(ui, dr, &format!("±{:.0} dB", rng), false, "Display range: click to cycle 3 / 6 / 12 / 30 dB.").clicked() {
                    let next = match p.disp.value() {
                        DispRange::D3 => DispRange::D6,
                        DispRange::D6 => DispRange::D12,
                        DispRange::D12 => DispRange::D30,
                        DispRange::D30 => DispRange::D3,
                    };
                    set_e(setter, &p.disp, next);
                }

                // ---------------- band panel
                if let Some(i) = st.focus.filter(|&i| views[i].used) {
                    let b = &p.bands[i];
                    let bv = views[i];
                    let col = band_col(i);
                    let has_gain = bv.spec.shape.has_gain();
                    let expanded = has_gain;
                    let _ = node_pos;
                    let pr = rect(14.0, 476.0, BASE_W - 28.0, 128.0);
                    painter.rect_filled(pr, CornerRadius::same(6), PANEL);
                    painter.rect_stroke(pr, CornerRadius::same(6), Stroke::new(1.0, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 120)), StrokeKind::Inside);
                    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + y), Vec2::new(w, h));
                    // column 1: band number, on, audition, delete
                    let used_idx: Vec<usize> = {
                        let mut v: Vec<usize> = (0..NB).filter(|&k| views[k].used).collect();
                        v.sort_by(|&a, &c| views[a].spec.freq.partial_cmp(&views[c].spec.freq).unwrap_or(std::cmp::Ordering::Equal));
                        v
                    };
                    let pos_in = used_idx.iter().position(|&k| k == i).unwrap_or(0);
                    if tbtn(ui, at(10.0, 10.0, 22.0, 22.0), "◀", false, "Previous band").clicked() && !used_idx.is_empty() {
                        let k = used_idx[(pos_in + used_idx.len() - 1) % used_idx.len()];
                        st.focus = Some(k);
                        st.sel = [false; NB];
                        st.sel[k] = true;
                    }
                    painter.text(at(34.0, 10.0, 36.0, 22.0).center(), Align2::CENTER_CENTER, format!("{:02}", i + 1), mono(13.0), col);
                    if tbtn(ui, at(72.0, 10.0, 22.0, 22.0), "▶", false, "Next band").clicked() && !used_idx.is_empty() {
                        let k = used_idx[(pos_in + 1) % used_idx.len()];
                        st.focus = Some(k);
                        st.sel = [false; NB];
                        st.sel[k] = true;
                    }
                    let on = b.on.value();
                    if tbtn(ui, at(10.0, 40.0, 84.0, 22.0), if on { "ON" } else { "OFF" }, on, "Enable / bypass this band (Alt-click the dot does the same).").clicked() {
                        set_b(setter, &b.on, !on);
                    }
                    let ear = tbtn(ui, at(10.0, 68.0, 84.0, 22.0), "LISTEN", sh.solo.load(Ordering::Relaxed) == i as i32, "Hold to hear only the part of the spectrum this band works on.");
                    let holding = ear.is_pointer_button_down_on();
                    sh.solo.store(if holding { i as i32 } else if sh.solo.load(Ordering::Relaxed) == i as i32 { -1 } else { sh.solo.load(Ordering::Relaxed) }, Ordering::Relaxed);
                    if tbtn(ui, at(10.0, 96.0, 84.0, 22.0), "DELETE", false, "Remove this band (or double-click its dot).").clicked() {
                        delete_band(setter, b);
                        st.focus = None;
                        st.sel[i] = false;
                    }
                    // column 2: shape, slope, placement
                    let shape_items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.short())).collect();
                    let sr = at(104.0, 10.0, 132.0, 30.0);
                    menu_btn(ui, setter, &b.shape, sr, "", &shape_items, "Filter shape");
                    shape_icon(&painter, Rect::from_min_size(sr.min + Vec2::new(8.0, 5.0), Vec2::new(30.0, 20.0)), bv.spec.shape, col);
                    painter.text(Pos2::new(sr.left() + 44.0, sr.center().y), Align2::LEFT_CENTER, bv.spec.shape.short(), mono(10.5), WHITE);
                    if bv.spec.shape.has_slope() {
                        let slope_items: Vec<(Slope, String)> = Slope::ALL.iter().filter(|s| s.for_shape(bv.spec.shape) == **s).map(|s| (*s, if *s == Slope::Brick { "Brickwall".to_string() } else { format!("{} dB/oct", s.label()) })).collect();
                        let si: Vec<(Slope, &str)> = slope_items.iter().map(|(s, n)| (*s, n.as_str())).collect();
                        let lab = if bv.spec.slope == Slope::Brick { "BRICKWALL".to_string() } else { format!("{} dB/oct", bv.spec.slope.label()) };
                        menu_btn(ui, setter, &b.slope, at(104.0, 48.0, 132.0, 24.0), &lab, &si, "Slope (steepness)");
                    }
                    let pl_items = [(Place::Stereo, "Stereo"), (Place::Left, "Left"), (Place::Right, "Right"), (Place::Mid, "Mid"), (Place::Side, "Side")];
                    menu_btn(ui, setter, &b.place, at(104.0, 80.0, 132.0, 24.0), &format!("{:?}", bv.place).to_uppercase(), &pl_items, "Which channels the band works on: both, left, right, mid (centre) or side.");
                    // knobs
                    let kr = 21.0;
                    let cell = |x: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + 8.0), Vec2::new(78.0, 100.0));
                    knob_ac(ui, setter, &b.freq, "FREQ", cell(246.0), "Frequency, 10 Hz to 30 kHz.", false, kr, col);
                    let qx = if has_gain { 406.0 } else { 326.0 };
                    if has_gain {
                        let gc = cell(326.0);
                        knob_ac(ui, setter, &b.gain, "GAIN", gc, "Gain, ±30 dB.", true, kr, col);
                        // the dynamic range as an outer ring of the gain knob
                        if bv.dynamic {
                            let c = Pos2::new(gc.center().x, gc.top() + kr + 4.0);
                            let a0 = std::f32::consts::PI * 0.75;
                            let sweep = std::f32::consts::PI * 1.5;
                            let nrm = |db: f32| ((db + 30.0) / 60.0).clamp(0.0, 1.0);
                            let (t0, t1) = (nrm(b.gain.value()), nrm(b.gain.value() + b.dyn_range.value()));
                            let n = 40;
                            let pts: Vec<Pos2> = (0..=n)
                                .map(|k| {
                                    let t = t0 + (t1 - t0) * k as f32 / n as f32;
                                    let a = a0 + t * sweep;
                                    c + Vec2::new(a.cos(), a.sin()) * (kr + 3.5)
                                })
                                .collect();
                            painter.add(egui::Shape::line(pts, PathStroke::new(2.5, C_DYN)));
                            let tn = nrm(b.gain.value() + bv.dyn_now / (p.gain_scale.value() / 100.0).abs().max(0.05));
                            let a = a0 + tn * sweep;
                            painter.circle_filled(c + Vec2::new(a.cos(), a.sin()) * (kr + 3.5), 3.0, WHITE);
                        }
                        let gq = b.gq.value();
                        if bv.spec.shape == Shape::Bell && tbtn(ui, at(400.0, 6.0, 26.0, 16.0), "GQ", gq, "Gain-Q interaction: the bell gets narrower the more gain it has.").clicked() {
                            set_b(setter, &b.gq, !gq);
                        }
                    }
                    if bv.spec.shape.has_q() {
                        knob_ac(ui, setter, &b.q, "Q", cell(qx), "Bandwidth / resonance. Mouse wheel over the dot does the same.", false, kr, col);
                    }
                    if has_gain {
                        knob_ac(ui, setter, &b.dyn_range, "RANGE", cell(486.0), "Dynamic range: how far the gain may move with the signal. Negative = cut when loud (compress), positive = boost when loud (expand), 0 = static band. Alt + wheel over the dot, or drag the yellow handle.", true, kr, C_DYN);
                    }
                    if expanded {
                        let x0 = 600.0;
                        painter.vline(pr.left() + x0 - 6.0, pr.top() + 10.0..=pr.bottom() - 10.0, Stroke::new(1.0, LINE));
                        let lvl = Shared::get(&sh.lvl_db[i]);
                        let thr_now = Shared::get(&sh.thr_db[i]);
                        painter.text(at(x0, 2.0, 34.0, 12.0).center(), Align2::CENTER_CENTER, "THR", mono(9.0), DIM);
                        thr_slider(ui, setter, b, at(x0 + 6.0, 16.0, 22.0, 88.0), lvl, thr_now);
                        let auto = b.thr_auto.value();
                        if tbtn(ui, at(x0 + 34.0, 16.0, 22.0, 18.0), "A", auto, "Automatic threshold (follows the band's own average level).").clicked() {
                            set_b(setter, &b.thr_auto, !auto);
                        }
                        let small = |x: f32| Rect::from_min_size(Pos2::new(pr.left() + x, pr.top() + 14.0), Vec2::new(64.0, 96.0));
                        knob_ac(ui, setter, &b.attack, "ATTACK", small(x0 + 58.0), "Attack: 50 % = automatic (set from the band's frequency), lower = faster, higher = slower.", true, 16.0, C_DYN);
                        knob_ac(ui, setter, &b.release, "RELEASE", small(x0 + 122.0), "Release: 50 % = automatic, lower = faster, higher = slower.", true, 16.0, C_DYN);
                        let free = b.sc_free.value();
                        if tbtn(ui, at(x0 + 192.0, 14.0, 64.0, 22.0), if free { "FREE" } else { "BAND" }, free, "Trigger: BAND = the dynamics listen to the band's own frequency range; FREE = your own low / high cut.").clicked() {
                            set_b(setter, &b.sc_free, !free);
                        }
                        let ext = b.sc_ext.value();
                        let has_sc = sh.has_sc.load(Ordering::Relaxed);
                        if tbtn(ui, at(x0 + 192.0, 42.0, 64.0, 22.0), "EXT SC", ext, if has_sc { "Trigger from the external side-chain input." } else { "Trigger from the external side-chain input (nothing is routed to it right now)." }).clicked() {
                            set_b(setter, &b.sc_ext, !ext);
                        }
                        let don = b.dyn_on.value();
                        if tbtn(ui, at(x0 + 192.0, 70.0, 64.0, 22.0), if don { "DYN ON" } else { "DYN OFF" }, don, "Bypass the dynamics only (the range is kept).").clicked() {
                            set_b(setter, &b.dyn_on, !don);
                        }
                        if tbtn(ui, at(x0 + 192.0, 98.0, 64.0, 22.0), "CLEAR", false, "Clear the dynamics: back to a static band.").clicked() {
                            set_f(setter, &b.dyn_range, 0.0);
                        }
                        if free {
                            knob_ac(ui, setter, &b.sc_lo, "SC LO", small(x0 + 262.0), "Side-chain low cut.", false, 16.0, C_DYN);
                            knob_ac(ui, setter, &b.sc_hi, "SC HI", small(x0 + 322.0), "Side-chain high cut.", false, 16.0, C_DYN);
                        } else {
                            painter.text(at(x0 + 262.0, 40.0, 120.0, 40.0).center(), Align2::CENTER_CENTER, "trigger:\nband range", mono(9.5), DIM);
                        }
                    }
                    if !has_gain {
                        painter.text(Pos2::new(pr.left() + (if bv.spec.shape.has_q() { 420.0 } else { 340.0 }), pr.center().y), Align2::LEFT_CENTER, "no gain / dynamics for this shape", mono(10.0), DIM);
                    }
                    if sh.dyn_blocked.load(Ordering::Relaxed) && bv.dynamic {
                        painter.text(Pos2::new(pr.right() - 12.0, pr.bottom() - 10.0), Align2::RIGHT_CENTER, "dynamics are off: Linear Phase above High resolution", mono(9.5), C_DYN);
                    }
                } else {
                    let pr = rect(14.0, 476.0, BASE_W - 28.0, 128.0);
                    panel(ui, pr);
                    painter.text(pr.center() - Vec2::new(0.0, 10.0), Align2::CENTER_CENTER, "double-click the display to add a band, click a dot to edit it", mono(11.0), DIM);
                    painter.text(pr.center() + Vec2::new(0.0, 12.0), Align2::CENTER_CENTER, "drag: frequency / gain   ·   wheel: Q   ·   Alt + wheel: dynamic range   ·   right-click: menu   ·   double-click a dot: delete", mono(9.5), DIM);
                }

                // ---------------- bottom bar
                let by = 612.0;
                let bar = rect(14.0, by, BASE_W - 28.0, 80.0);
                panel(ui, bar);
                let mode_items = [(PMode::Zero, "ZERO LATENCY"), (PMode::Natural, "NATURAL PHASE"), (PMode::Linear, "LINEAR PHASE")];
                let mode_lab = mode_items.iter().find(|(m, _)| *m == p.pmode.value()).map(|x| x.1).unwrap_or("");
                painter.text(Pos2::new(o.x + 28.0, o.y + by + 14.0), Align2::LEFT_CENTER, "PROCESSING", mono(9.0), DIM);
                menu_btn(ui, setter, &p.pmode, rect(28.0, by + 24.0, 150.0, 26.0), mode_lab, &mode_items, "Zero Latency: analog-matched filters, no delay. Natural Phase: also matches the analog phase, a few ms of latency. Linear Phase: no phase shift at all, more latency (and pre-ringing).");
                if p.pmode.value() == PMode::Linear {
                    let res_items = [(Res::Low, "LOW"), (Res::Medium, "MEDIUM"), (Res::High, "HIGH"), (Res::VeryHigh, "VERY HIGH"), (Res::Max, "MAXIMUM")];
                    let rl = res_items.iter().find(|(m, _)| *m == p.res.value()).map(|x| x.1).unwrap_or("");
                    menu_btn(ui, setter, &p.res, rect(184.0, by + 24.0, 104.0, 26.0), rl, &res_items, "Linear phase resolution: higher = better low end, more latency. Dynamic bands work up to High.");
                }
                let lat = sh.latency.load(Ordering::Relaxed);
                painter.text(Pos2::new(o.x + 28.0, o.y + by + 64.0), Align2::LEFT_CENTER, format!("latency {:.1} ms", lat as f32 / fs * 1000.0), mono(9.5), DIM);

                painter.text(Pos2::new(o.x + 306.0, o.y + by + 14.0), Align2::LEFT_CENTER, "CHARACTER", mono(9.0), DIM);
                let ch_items = [(Character::Clean, "CLEAN"), (Character::Subtle, "SUBTLE"), (Character::Warm, "WARM")];
                let cl = ch_items.iter().find(|(m, _)| *m == p.character.value()).map(|x| x.1).unwrap_or("");
                menu_btn(ui, setter, &p.character, rect(306.0, by + 24.0, 100.0, 26.0), cl, &ch_items, "Clean: no colour. Subtle / Warm: analog-style saturation (oversampled).");

                painter.text(Pos2::new(o.x + 424.0, o.y + by + 14.0), Align2::LEFT_CENTER, "ANALYZER", mono(9.0), DIM);
                for (k, (bp, name, tip)) in [(&p.an_pre, "PRE", "Spectrum before the EQ"), (&p.an_post, "POST", "Spectrum after the EQ"), (&p.an_sc, "SC", "Side-chain spectrum")].into_iter().enumerate() {
                    let v = bp.value();
                    if tbtn(ui, rect(424.0 + k as f32 * 50.0, by + 24.0, 46.0, 26.0), name, v, tip).clicked() {
                        set_b(setter, bp, !v);
                    }
                }

                // output section
                let ag = p.auto_gain.value();
                if tbtn(ui, rect(600.0, by + 24.0, 90.0, 26.0), "AUTO GAIN", ag, "Keeps the overall level about the same whatever the EQ curve does.").clicked() {
                    set_b(setter, &p.auto_gain, !ag);
                }
                let ph = p.phase.value();
                if tbtn(ui, rect(696.0, by + 24.0, 34.0, 26.0), "Ø", ph, "Invert the output polarity.").clicked() {
                    set_b(setter, &p.phase, !ph);
                }
                let kc = |x: f32| Rect::from_min_size(Pos2::new(o.x + x, o.y + by + 2.0), Vec2::new(70.0, 78.0));
                knob_r(ui, setter, &p.gain_scale, "SCALE", kc(744.0), "Gain scale: scales every band's gain and dynamic range (negative turns the curve upside down).", false, 14.0);
                knob_r(ui, setter, &p.out, "OUTPUT", kc(814.0), "Output gain.", true, 14.0);
                knob_r(ui, setter, &p.pan, "PAN", kc(884.0), "Output balance.", true, 14.0);
                for c in 0..2 {
                    let mr = rect(972.0 + c as f32 * 12.0, by + 10.0, 9.0, 60.0);
                    vbar(ui, mr, st.out_pk[c], st.out_pk[c], -48.0, 3.0);
                }
                painter.text(Pos2::new(o.x + 1028.0, o.y + by + 40.0), Align2::CENTER_CENTER, format!("{:.1}", st.out_pk[0].max(st.out_pk[1]).max(-90.0)), mono(9.5), if st.out_pk[0].max(st.out_pk[1]) > 0.0 { Color32::from_rgb(230, 90, 80) } else { DIM });

                resize_grip(ui, ctx, &size_state, full, BASE_W, BASE_H, 760.0);
            });
        },
    )
}
