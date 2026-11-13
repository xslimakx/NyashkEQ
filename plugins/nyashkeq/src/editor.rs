//! NyashkEQ GUI: draggable response graph over a live spectrum, band knobs underneath.
use nih_plug::prelude::*;
use nih_plug_egui::{
    create_egui_editor,
    egui::{self, epaint::PathStroke, Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2},
};
use nyashk_common::eq::*;
use nyashk_common::ui::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::{fmt_hz, EqParams, Mode, Shared, Voice};

pub const BASE_W: f32 = 940.0;
pub const BASE_H: f32 = 560.0;
const F_LO: f32 = 20.0;
const F_HI: f32 = 20000.0;
const D_RANGE: f32 = 18.0;
const FFT_N: usize = 4096;

struct Gui {
    an: nyashk_common::analyzer::Spectrum,
    out_pk: f32,
}

fn fx(g: Rect, f: f32) -> f32 {
    g.left() + g.width() * (f / F_LO).ln() / (F_HI / F_LO).ln()
}
fn dy(g: Rect, d: f32) -> f32 {
    g.center().y - g.height() * 0.5 * (d / D_RANGE).clamp(-1.0, 1.0)
}
fn x_to_f(g: Rect, x: f32) -> f32 {
    F_LO * (F_HI / F_LO).powf(((x - g.left()) / g.width()).clamp(0.0, 1.0))
}
fn y_to_d(g: Rect, y: f32) -> f32 {
    ((g.center().y - y) / (g.height() * 0.5) * D_RANGE).clamp(-D_RANGE, D_RANGE)
}

/// Drag one graph node: horizontal = frequency, vertical = gain (optional).
#[allow(clippy::too_many_arguments)]
fn node(ui: &mut Ui2, setter: &ParamSetter, g: Rect, id: &str, f: &FloatParam, gp: Option<&FloatParam>, q: Option<&FloatParam>, label: &str, on: bool) {
    let fv = f.value();
    let d = gp.map(|p| p.value()).unwrap_or(0.0);
    // drawn inside the frame: at 20 Hz / 20 kHz / +-18 dB the point used to sit half on the border
    let inner = g.shrink(9.0);
    let pos = Pos2::new(fx(g, fv).clamp(inner.left(), inner.right()), dy(g, d).clamp(inner.top(), inner.bottom()));
    let hit = Rect::from_center_size(pos, Vec2::splat(22.0));
    let resp = ui.interact(hit, egui::Id::new(("eqnode", id)), Sense::click_and_drag());
    if resp.drag_started() {
        setter.begin_set_parameter(f);
        if let Some(gp) = gp {
            setter.begin_set_parameter(gp);
        }
    }
    if resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            let nf = x_to_f(g, pp.x);
            setter.set_parameter_normalized(f, f.preview_normalized(nf));
            if let Some(gp) = gp {
                let nd = y_to_d(g, pp.y);
                setter.set_parameter_normalized(gp, gp.preview_normalized(nd));
            }
        }
    }
    if resp.drag_stopped() {
        setter.end_set_parameter(f);
        if let Some(gp) = gp {
            setter.end_set_parameter(gp);
        }
    }
    if resp.double_clicked() {
        if let Some(gp) = gp {
            set_norm(setter, gp, gp.default_normalized_value());
        }
    }
    if let (Some(q), true) = (q, resp.hovered()) {
        let sc = ui.input(|i| i.smooth_scroll_delta.y);
        if sc != 0.0 {
            set_norm(setter, q, q.unmodulated_normalized_value() + sc / 900.0);
        }
    }
    let hot = resp.hovered() || resp.dragged();
    let painter = ui.painter();
    let accent = match id {
        "lf" => C_LF,
        "lmf" => C_LMF,
        "hmf" => C_HMF,
        "hf" => C_HF,
        _ => WHITE,
    };
    let col = if on { accent } else { DIM };
    painter.circle_filled(pos, if hot { 8.0 } else { 6.0 }, if on { col } else { BG });
    painter.circle_stroke(pos, if hot { 8.0 } else { 6.0 }, Stroke::new(1.5, col));
    // label above the point (below it when there is no room), value on the other side; both kept
    // inside the frame horizontally
    // `off`: extra distance from the point (to stack the value under / over the label)
    let put = |text: String, size: f32, colour: Color32, above: bool, off: f32| {
        let gal = painter.layout_no_wrap(text, mono(size), colour);
        let sz = gal.size();
        let x = (pos.x - sz.x * 0.5).clamp(g.left() + 3.0, g.right() - 3.0 - sz.x);
        let y = if above { pos.y - 11.0 - off - sz.y } else { pos.y + 11.0 + off };
        // dark backing: readable where the curve or the spectrum runs through the text
        painter.rect_filled(Rect::from_min_size(Pos2::new(x, y), sz).expand2(Vec2::new(3.0, 1.0)), 2.0, Color32::from_black_alpha(170));
        painter.galley(Pos2::new(x, y), gal, colour);
    };
    let room_above = pos.y - 11.0 - 12.0 >= g.top() + 2.0;
    let room_below = pos.y + 11.0 + 13.0 <= g.bottom() - 2.0;
    let label_above = room_above || !room_below;
    put(label.to_string(), 9.5, if hot { WHITE } else { DIM }, label_above, 0.0);
    if hot {
        let t = match gp {
            Some(_) => format!("{} Hz  {:+.1} dB", fmt_hz(fv), d),
            None => format!("{} Hz", fmt_hz(fv)),
        };
        // the value goes on the other side of the point; if that side has no room, it is stacked
        // next to the label instead (never outside the graph)
        let other_room = if label_above { room_below } else { room_above };
        if other_room {
            put(t, 10.0, WHITE, !label_above, 0.0);
        } else {
            put(t, 10.0, WHITE, label_above, 15.0);
        }
    }
}
type Ui2 = nih_plug_egui::egui::Ui;

pub fn create(params: Arc<EqParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    let editor_state = params.editor_state.clone();
    let size_state = editor_state.clone();
    create_egui_editor(
        editor_state,
        Gui { an: nyashk_common::analyzer::Spectrum::new(FFT_N), out_pk: -90.0 },
        |ctx, _| apply_theme(ctx),
        move |ctx, setter, st| {
            ctx.request_repaint_after(Duration::from_millis(33));
            let p = &*params;
            let fs = f32::from_bits(shared.fs.load(Ordering::Relaxed));
            st.an.update(&shared.tap);
            let o_db = f32::from_bits(shared.out_db.load(Ordering::Relaxed));
            st.out_pk = if o_db > st.out_pk { o_db } else { st.out_pk - 0.9 };

            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(BG)).show(ctx, |ui| {
                let full = ui.max_rect();
                let o = full.min;
                let rect = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(Pos2::new(o.x + x, o.y + y), Vec2::new(w, h));
                header(ui, o, BASE_W, "EQ", "4-BAND PARAMETRIC EQUALIZER");
                let byp = p.bypass.value();
                if pill_tip(ui, rect(BASE_W - 14.0 - 76.0, 22.0, 76.0, 24.0), "BYPASS", byp, "Bypass the whole plugin.") {
                    set_b(setter, &p.bypass, !byp);
                }

                // ---------------- graph
                let gp = rect(14.0, 70.0, BASE_W - 28.0, 272.0);
                panel(ui, gp);
                let g = Rect::from_min_max(Pos2::new(gp.left() + 36.0, gp.top() + 12.0), Pos2::new(gp.right() - 44.0, gp.bottom() - 22.0));
                let painter = ui.painter().clone();
                painter.rect_filled(g, 4.0, Color32::from_rgb(0x07, 0x07, 0x07));
                let clip = painter.with_clip_rect(g);
                for d in [-12.0f32, -6.0, 0.0, 6.0, 12.0] {
                    painter.hline(g.left()..=g.right(), dy(g, d), Stroke::new(1.0, if d == 0.0 { MID } else { LINE }));
                    painter.text(Pos2::new(g.left() - 6.0, dy(g, d)), Align2::RIGHT_CENTER, format!("{:+.0}", d), mono(9.5), DIM);
                }
                for f in [20.0f32, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
                    painter.vline(fx(g, f), g.top()..=g.bottom(), Stroke::new(1.0, LINE));
                    if f < 15000.0 {
                        painter.text(Pos2::new(fx(g, f), g.bottom() + 11.0), Align2::CENTER_CENTER, fmt_hz(f), mono(9.5), DIM);
                    }
                }
                // spectrum (output), tilted +4.5 dB/oct so pink noise looks flat; one column per
                // pixel (loudest bin), solid fill without overlaps, grid redrawn on top
                let spec_pts = st.an.points(fs, F_LO, F_HI.min(fs * 0.49), 260, 4.5, |f| fx(g, f), |db| g.bottom() - g.height() * 0.8 * ((db + 18.0 + 90.0) / 90.0).clamp(0.0, 1.0));
                nyashk_common::analyzer::draw_spectrum(&clip, g, &spec_pts, Color32::from_gray(30), Color32::from_gray(85));
                for d in [-12.0f32, -6.0, 6.0, 12.0] {
                    clip.hline(g.left()..=g.right(), dy(g, d), Stroke::new(1.0, Color32::from_white_alpha(8)));
                }
                for f in [50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
                    clip.vline(fx(g, f), g.top()..=g.bottom(), Stroke::new(1.0, Color32::from_white_alpha(8)));
                }
                // response curve
                let cfg = p.cfg();
                let cf = EqCoefs::new(fs, &cfg);
                let on = p.eq_in.value() && !byp;
                let pts: Vec<Pos2> = (0..=240)
                    .map(|i| {
                        let f = F_LO * (F_HI / F_LO).powf(i as f32 / 240.0);
                        Pos2::new(fx(g, f), dy(g, if on { cf.response_db(fs, f) } else { 0.0 }))
                    })
                    .collect();
                fill_under(&clip, &pts, dy(g, 0.0), Color32::from_white_alpha(16));
                clip.add(egui::Shape::line(pts, PathStroke::new(2.2, WHITE)));

                // nodes
                node(ui, setter, g, "hp", &p.hp_f, None, None, "HP", p.hp_on.value());
                node(ui, setter, g, "lf", &p.lf_f, Some(&p.lf_g), None, "LF", true);
                node(ui, setter, g, "lmf", &p.lmf_f, Some(&p.lmf_g), Some(&p.lmf_q), "LMF", true);
                node(ui, setter, g, "hmf", &p.hmf_f, Some(&p.hmf_g), Some(&p.hmf_q), "HMF", true);
                node(ui, setter, g, "hf", &p.hf_f, Some(&p.hf_g), None, "HF", true);
                node(ui, setter, g, "lp", &p.lp_f, None, None, "LP", p.lp_on.value());
                // hint in the header: inside the graph the points ran over it
                painter.text(Pos2::new(BASE_W - 14.0 - 76.0 - 14.0, 34.0), Align2::RIGHT_CENTER, "drag the points  ·  wheel on LMF / HMF = width  ·  double-click = reset gain", mono(9.0), DIM);

                // output meter on the right of the graph
                let mr = Rect::from_min_size(Pos2::new(gp.right() - 30.0, g.top()), Vec2::new(14.0, g.height()));
                vbar(ui, mr, st.out_pk, st.out_pk, -48.0, 0.0);
                painter.text(Pos2::new(mr.center().x, mr.bottom() + 11.0), Align2::CENTER_CENTER, "OUT", mono(9.5), DIM);

                // ---------------- switches strip
                let sy = 350.0;
                let x = std::cell::Cell::new(14.0f32);
                let sw = |ui: &mut Ui2, w: f32, text: &str, on: bool, tip: &str| -> bool {
                    let r = rect(x.get(), sy, w, 24.0);
                    x.set(x.get() + w + 6.0);
                    pill_tip(ui, r, text, on, tip)
                };
                let ein = p.eq_in.value();
                if sw(ui, 72.0, "EQ IN", ein, "Switch the equalizer section on or off (the output gain stays active).") {
                    set_b(setter, &p.eq_in, !ein);
                }
                let ph = p.phase.value();
                if sw(ui, 72.0, "PHASE", ph, "Invert the polarity of the output.") {
                    set_b(setter, &p.phase, !ph);
                }
                let an = p.analog.value();
                if sw(ui, 72.0, "ANALOG", an, "Gentle tube-style saturation on the output (adds even harmonics, oversampled).") {
                    set_b(setter, &p.analog, !an);
                }
                x.set(x.get() + 10.0);
                painter.text(Pos2::new(o.x + x.get(), o.y + sy + 12.0), Align2::LEFT_CENTER, "PROCESS", mono(9.5), DIM);
                x.set(x.get() + 62.0);
                for (m, tip) in [(Mode::Stereo, "Left and right."), (Mode::Mid, "Only the centre (mid) is equalized; the sides stay untouched."), (Mode::Side, "Only the sides are equalized; the centre stays untouched.")] {
                    let cur = p.mode.value() == m;
                    let name = match m {
                        Mode::Stereo => "STEREO",
                        Mode::Mid => "MID",
                        Mode::Side => "SIDE",
                    };
                    if sw(ui, 66.0, name, cur, tip) {
                        set_e(setter, &p.mode, m);
                    }
                }
                x.set(x.get() + 10.0);
                painter.text(Pos2::new(o.x + x.get(), o.y + sy + 12.0), Align2::LEFT_CENTER, "VOICING", mono(9.5), DIM);
                x.set(x.get() + 62.0);
                for (v, name, tip) in [
                    (Voice::Black, "BLACK", "Constant-Q bells and tighter shelves: precise, surgical."),
                    (Voice::Brown, "BROWN", "Proportional-Q bells (narrower with more gain) and broader shelves: musical, forgiving."),
                ] {
                    if sw(ui, 66.0, name, p.voice.value() == v, tip) {
                        set_e(setter, &p.voice, v);
                    }
                }

                // ---------------- knobs
                let kp = rect(14.0, 382.0, BASE_W - 28.0, 168.0);
                panel(ui, kp);
                let cw = 56.0;
                let r = 22.0;
                let mut cx = kp.left() + 10.0;
                let top = kp.top() + 30.0;
                let cell = |x: f32| Rect::from_min_size(Pos2::new(x, top), Vec2::new(cw, 88.0));
                let title = |ui: &Ui2, x: f32, w: f32, t: &str| {
                    ui.painter().text(Pos2::new(x + w * 0.5, kp.top() + 14.0), Align2::CENTER_CENTER, t, mono(10.5), WHITE);
                    ui.painter().hline(x + 4.0..=x + w - 4.0, kp.top() + 24.0, Stroke::new(1.0, LINE));
                };
                let gap = 12.0;
                // HP
                title(ui, cx, cw, "HP");
                knob_r(ui, setter, &p.hp_f, "FREQ", cell(cx), "High-pass cut-off (18 dB/oct), 16 to 350 Hz.", false, r);
                let hon = p.hp_on.value();
                if pill_tip(ui, Rect::from_min_size(Pos2::new(cx + 4.0, top + 96.0), Vec2::new(cw - 8.0, 22.0)), "ON", hon, "Switch the high-pass filter on or off.") {
                    set_b(setter, &p.hp_on, !hon);
                }
                cx += cw + gap;
                // bands
                let mut band = |ui: &mut Ui2, cx: &mut f32, name: &str, g: &FloatParam, f: &FloatParam, third: Option<(&FloatParam, &str, &str)>, bell: Option<&BoolParam>, tips: (&str, &str)| {
                    let acc = match name {
                        "LF" => C_LF,
                        "LMF" => C_LMF,
                        "HMF" => C_HMF,
                        _ => C_HF,
                    };
                    let w = if third.is_some() { cw * 3.0 } else { cw * 2.0 };
                    title(ui, *cx, w, name);
                    knob_ac(ui, setter, g, "GAIN", cell(*cx), tips.0, true, r, acc);
                    knob_ac(ui, setter, f, "FREQ", cell(*cx + cw), tips.1, false, r, acc);
                    if let Some((q, lbl, tip)) = third {
                        knob_ac(ui, setter, q, lbl, cell(*cx + 2.0 * cw), tip, false, r, acc);
                    }
                    if let Some(b) = bell {
                        let v = b.value();
                        if pill_tip(ui, Rect::from_min_size(Pos2::new(*cx + cw + 4.0, top + 96.0), Vec2::new(cw - 8.0, 22.0)), "BELL", v, "BELL: peaking curve. Off = shelf (raises or lowers everything beyond the frequency).") {
                            set_b(setter, b, !v);
                        }
                    }
                    *cx += w + gap;
                };
                band(ui, &mut cx, "LF", &p.lf_g, &p.lf_f, None, Some(&p.lf_bell), ("Low band gain, +/- 16 dB.", "Low band frequency, 30 to 450 Hz."));
                band(ui, &mut cx, "LMF", &p.lmf_g, &p.lmf_f, Some((&p.lmf_q, "Q", "Width of the bell: low = wide and gentle, high = narrow and surgical.")), None, ("Low-mid gain, +/- 16 dB.", "Low-mid frequency, 200 Hz to 2.5 kHz."));
                band(ui, &mut cx, "HMF", &p.hmf_g, &p.hmf_f, Some((&p.hmf_q, "Q", "Width of the bell: low = wide and gentle, high = narrow and surgical.")), None, ("High-mid gain, +/- 16 dB.", "High-mid frequency, 600 Hz to 7 kHz."));
                band(ui, &mut cx, "HF", &p.hf_g, &p.hf_f, None, Some(&p.hf_bell), ("High band gain, +/- 16 dB.", "High band frequency, 1.5 to 16 kHz."));
                // LP
                title(ui, cx, cw, "LP");
                knob_r(ui, setter, &p.lp_f, "FREQ", cell(cx), "Low-pass cut-off (12 dB/oct), 3 to 22 kHz.", false, r);
                let lon = p.lp_on.value();
                if pill_tip(ui, Rect::from_min_size(Pos2::new(cx + 4.0, top + 96.0), Vec2::new(cw - 8.0, 22.0)), "ON", lon, "Switch the low-pass filter on or off.") {
                    set_b(setter, &p.lp_on, !lon);
                }
                cx += cw + gap;
                title(ui, cx, cw, "OUT");
                knob_r(ui, setter, &p.out, "GAIN", cell(cx), "Output gain in dB.", true, r);

                resize_grip(ui, ctx, &size_state, full, BASE_W, BASE_H, 640.0);
            });
        },
    )
}
