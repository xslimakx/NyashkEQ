//! NyashkEQ: 4-band parametric equalizer with high-pass / low-pass, live spectrum and a draggable response graph.

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use nyashk_common::dsp::*;
use nyashk_common::eq::*;
use nyashk_common::tap::Tap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

mod editor;

pub struct Shared {
    pub tap: Tap,
    pub out_db: AtomicU32,
    pub fs: AtomicU32,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Mode {
    #[name = "STEREO"]
    Stereo,
    #[name = "MID"]
    Mid,
    #[name = "SIDE"]
    Side,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Voice {
    #[name = "BLACK"]
    Black,
    #[name = "BROWN"]
    Brown,
}

pub fn fmt_hz(v: f32) -> String {
    if v >= 1000.0 {
        format!("{:.2}k", v / 1000.0)
    } else {
        format!("{:.0}", v)
    }
}
pub fn parse_hz(s: &str) -> Option<f32> {
    let t = s.trim().to_lowercase().replace("hz", "");
    let t = t.trim();
    if let Some(k) = t.strip_suffix('k') {
        k.trim().parse::<f32>().ok().map(|v| v * 1000.0)
    } else {
        t.parse().ok()
    }
}

fn freq(name: &str, def: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, def, FloatRange::Skewed { min, max, factor: FloatRange::skew_factor(-2.0) })
        .with_smoother(SmoothingStyle::Logarithmic(25.0))
        .with_value_to_string(Arc::new(fmt_hz))
        .with_string_to_value(Arc::new(parse_hz))
}
fn gain(name: &str) -> FloatParam {
    FloatParam::new(name, 0.0, FloatRange::Linear { min: -16.0, max: 16.0 })
        .with_step_size(0.1)
        .with_smoother(SmoothingStyle::Linear(25.0))
        .with_value_to_string(Arc::new(|v| format!("{:+.1}", v)))
}
fn qp(name: &str, def: f32) -> FloatParam {
    FloatParam::new(name, def, FloatRange::Skewed { min: 0.3, max: 8.0, factor: FloatRange::skew_factor(-1.5) })
        .with_step_size(0.01)
        .with_smoother(SmoothingStyle::Linear(25.0))
        .with_value_to_string(Arc::new(|v| format!("{:.2}", v)))
}

#[derive(Params)]
pub struct EqParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    #[id = "bypass"]
    pub bypass: BoolParam,
    #[id = "eqin"]
    pub eq_in: BoolParam,
    #[id = "phase"]
    pub phase: BoolParam,
    #[id = "analog"]
    pub analog: BoolParam,
    #[id = "mode"]
    pub mode: EnumParam<Mode>,
    #[id = "voice"]
    pub voice: EnumParam<Voice>,
    #[id = "out"]
    pub out: FloatParam,

    #[id = "hpon"]
    pub hp_on: BoolParam,
    #[id = "hpf"]
    pub hp_f: FloatParam,
    #[id = "lpon"]
    pub lp_on: BoolParam,
    #[id = "lpf"]
    pub lp_f: FloatParam,

    #[id = "lfg"]
    pub lf_g: FloatParam,
    #[id = "lff"]
    pub lf_f: FloatParam,
    #[id = "lfb"]
    pub lf_bell: BoolParam,
    #[id = "lmg"]
    pub lmf_g: FloatParam,
    #[id = "lmf"]
    pub lmf_f: FloatParam,
    #[id = "lmq"]
    pub lmf_q: FloatParam,
    #[id = "hmg"]
    pub hmf_g: FloatParam,
    #[id = "hmf"]
    pub hmf_f: FloatParam,
    #[id = "hmq"]
    pub hmf_q: FloatParam,
    #[id = "hfg"]
    pub hf_g: FloatParam,
    #[id = "hff"]
    pub hf_f: FloatParam,
    #[id = "hfb"]
    pub hf_bell: BoolParam,
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(940, 560),
            bypass: BoolParam::new("Bypass", false).make_bypass(),
            eq_in: BoolParam::new("EQ In", true),
            phase: BoolParam::new("Phase Invert", false),
            analog: BoolParam::new("Analog", false),
            mode: EnumParam::new("Mode", Mode::Stereo),
            voice: EnumParam::new("Voicing", Voice::Black),
            out: FloatParam::new("Output", 0.0, FloatRange::Linear { min: -18.0, max: 18.0 })
                .with_step_size(0.1)
                .with_smoother(SmoothingStyle::Linear(25.0))
                .with_value_to_string(Arc::new(|v| format!("{:+.1}", v))),
            hp_on: BoolParam::new("HP On", false),
            hp_f: freq("HP Freq", 30.0, 16.0, 350.0),
            lp_on: BoolParam::new("LP On", false),
            lp_f: freq("LP Freq", 18000.0, 3000.0, 22000.0),
            lf_g: gain("LF Gain"),
            lf_f: freq("LF Freq", 100.0, 30.0, 450.0),
            lf_bell: BoolParam::new("LF Bell", false),
            lmf_g: gain("LMF Gain"),
            lmf_f: freq("LMF Freq", 600.0, 200.0, 2500.0),
            lmf_q: qp("LMF Q", 1.0),
            hmf_g: gain("HMF Gain"),
            hmf_f: freq("HMF Freq", 3000.0, 600.0, 7000.0),
            hmf_q: qp("HMF Q", 1.0),
            hf_g: gain("HF Gain"),
            hf_f: freq("HF Freq", 10000.0, 1500.0, 16000.0),
            hf_bell: BoolParam::new("HF Bell", false),
        }
    }
}

impl EqParams {
    /// Current (unsmoothed) settings as an EQ configuration; used by the DSP and by the GUI graph.
    pub fn cfg(&self) -> EqCfg {
        EqCfg {
            hp_on: self.hp_on.value(),
            hp_f: self.hp_f.value(),
            lp_on: self.lp_on.value(),
            lp_f: self.lp_f.value(),
            lf: Band { freq: self.lf_f.value(), gain_db: self.lf_g.value(), q: 1.0, bell: self.lf_bell.value() },
            lmf: Band { freq: self.lmf_f.value(), gain_db: self.lmf_g.value(), q: self.lmf_q.value(), bell: true },
            hmf: Band { freq: self.hmf_f.value(), gain_db: self.hmf_g.value(), q: self.hmf_q.value(), bell: true },
            hf: Band { freq: self.hf_f.value(), gain_db: self.hf_g.value(), q: 1.0, bell: self.hf_bell.value() },
            voicing: match self.voice.value() {
                Voice::Black => Voicing::Black,
                Voice::Brown => Voicing::Brown,
            },
        }
    }
}

pub struct Core {
    st: [EqState; 2],
    os: [Os2; 2],
    fs: f32,
    coefs: EqCoefs,
    tick: u32,
    pub out_peak: f32,
}

impl Core {
    pub fn new(fs: f32) -> Self {
        Self { st: Default::default(), os: [Os2::new(fs), Os2::new(fs)], fs, coefs: EqCoefs::new(fs, &EqCfg::default()), tick: 0, out_peak: 0.0 }
    }
    pub fn reset(&mut self) {
        for s in self.st.iter_mut() {
            s.reset();
        }
        for o in self.os.iter_mut() {
            o.reset();
        }
    }

    /// One frame. `cfg` is evaluated every 16 samples to update coefficients.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(&mut self, cfg: &EqCfg, eq_in: bool, mode: Mode, phase: bool, analog: bool, out_lin: f32, x: &mut [f32; 2], n: usize) {
        if self.tick % 16 == 0 {
            self.coefs = EqCoefs::new(self.fs, cfg);
        }
        self.tick = self.tick.wrapping_add(1);
        let c = self.coefs;
        let st = &mut self.st;
        if eq_in {
            if n == 2 {
                match mode {
                    Mode::Stereo => {
                        x[0] = st[0].tick(&c, x[0]);
                        x[1] = st[1].tick(&c, x[1]);
                    }
                    Mode::Mid | Mode::Side => {
                        let (m, s) = ((x[0] + x[1]) * 0.5, (x[0] - x[1]) * 0.5);
                        let (m, s) = if mode == Mode::Mid { (st[0].tick(&c, m), s) } else { (m, st[1].tick(&c, s)) };
                        x[0] = m + s;
                        x[1] = m - s;
                    }
                }
            } else {
                x[0] = st[0].tick(&c, x[0]);
            }
        }
        for i in 0..n {
            let mut v = x[i];
            if analog {
                v = self.os[i].process(v, |a| sat_asym(a * 0.8, 0.04) / 0.8);
            }
            if phase {
                v = -v;
            }
            x[i] = v * out_lin;
        }
    }
}

pub struct NyashkEQ {
    params: Arc<EqParams>,
    shared: Arc<Shared>,
    core: Option<Core>,
}

impl Default for NyashkEQ {
    fn default() -> Self {
        Self {
            params: Arc::new(EqParams::default()),
            shared: Arc::new(Shared { tap: Tap::new(16384), out_db: AtomicU32::new((-90f32).to_bits()), fs: AtomicU32::new(48000f32.to_bits()) }),
            core: None,
        }
    }
}

impl Plugin for NyashkEQ {
    const NAME: &'static str = "NyashkEQ";
    const VENDOR: &'static str = "Nyashk";
    const URL: &'static str = "https://github.com/xslimakx";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout { main_input_channels: NonZeroU32::new(2), main_output_channels: NonZeroU32::new(2), ..AudioIOLayout::const_default() },
        AudioIOLayout { main_input_channels: NonZeroU32::new(1), main_output_channels: NonZeroU32::new(1), ..AudioIOLayout::const_default() },
    ];
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _e: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.shared.clone())
    }

    fn initialize(&mut self, _l: &AudioIOLayout, config: &BufferConfig, _c: &mut impl InitContext<Self>) -> bool {
        self.core = Some(Core::new(config.sample_rate));
        self.shared.fs.store(config.sample_rate.to_bits(), Ordering::Relaxed);
        true
    }

    fn reset(&mut self) {
        if let Some(c) = self.core.as_mut() {
            c.reset();
        }
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, _c: &mut impl ProcessContext<Self>) -> ProcessStatus {
        nyashk_common::demo::sanitize(buffer);
        #[cfg(feature = "standalone")]
        nyashk_common::demo::fill(buffer, 48000.0);
        let Some(core) = self.core.as_mut() else { return ProcessStatus::Normal };
        let p = &self.params;
        if p.bypass.value() {
            for frame in buffer.iter_samples() {
                let n = frame.len().min(2);
                let m: f32 = frame.into_iter().take(n).map(|s| *s).sum::<f32>() / n.max(1) as f32;
                self.shared.tap.push(m);
            }
            return ProcessStatus::Normal;
        }
        let mut pk = 0.0f32;
        let mut cfg = p.cfg();
        for (k, mut frame) in buffer.iter_samples().enumerate() {
            // smoothed values for the filters, read every 16th frame (the coefficients are refreshed then)
            if k % 16 == 0 {
                cfg.hp_f = p.hp_f.smoothed.next_step(16);
                cfg.lp_f = p.lp_f.smoothed.next_step(16);
                cfg.lf.freq = p.lf_f.smoothed.next_step(16);
                cfg.lf.gain_db = p.lf_g.smoothed.next_step(16);
                cfg.lmf.freq = p.lmf_f.smoothed.next_step(16);
                cfg.lmf.gain_db = p.lmf_g.smoothed.next_step(16);
                cfg.lmf.q = p.lmf_q.smoothed.next_step(16);
                cfg.hmf.freq = p.hmf_f.smoothed.next_step(16);
                cfg.hmf.gain_db = p.hmf_g.smoothed.next_step(16);
                cfg.hmf.q = p.hmf_q.smoothed.next_step(16);
                cfg.hf.freq = p.hf_f.smoothed.next_step(16);
                cfg.hf.gain_db = p.hf_g.smoothed.next_step(16);
                cfg.hp_on = p.hp_on.value();
                cfg.lp_on = p.lp_on.value();
                cfg.lf.bell = p.lf_bell.value();
                cfg.hf.bell = p.hf_bell.value();
            }
            let out_lin = db_to_lin(p.out.smoothed.next());
            let n = frame.len().min(2);
            let mut x = [0.0f32; 2];
            for (i, s) in frame.iter_mut().enumerate().take(n) {
                x[i] = *s;
            }
            core.frame(&cfg, p.eq_in.value(), p.mode.value(), p.phase.value(), p.analog.value(), out_lin, &mut x, n);
            for (i, s) in frame.iter_mut().enumerate().take(n) {
                *s = x[i];
                pk = pk.max(x[i].abs());
            }
            self.shared.tap.push(if n == 2 { 0.5 * (x[0] + x[1]) } else { x[0] });
        }
        self.shared.out_db.store((20.0 * (pk + 1e-9).log10()).to_bits(), Ordering::Relaxed);
        ProcessStatus::Normal
    }
}

impl Vst3Plugin for NyashkEQ {
    const VST3_CLASS_ID: [u8; 16] = *b"NyashkEQVST3!!!!";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx, Vst3SubCategory::Eq];
}

nih_export_vst3!(NyashkEQ);
