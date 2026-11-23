//! Parameters: 24 bands (each with dynamics and side-chain settings) and the global controls.
use crate::conv::{Place, NB};
use crate::filters::{Shape, Slope};
use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::sync::Arc;

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum PMode {
    #[name = "Zero Latency"]
    Zero,
    #[name = "Natural Phase"]
    Natural,
    #[name = "Linear Phase"]
    Linear,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Res {
    #[name = "Low"]
    Low,
    #[name = "Medium"]
    Medium,
    #[name = "High"]
    High,
    #[name = "Very High"]
    VeryHigh,
    #[name = "Maximum"]
    Max,
}

impl Res {
    /// kernel length at 44.1 / 48 kHz
    pub fn n48(self) -> usize {
        match self {
            Res::Low => 4096,
            Res::Medium => 8192,
            Res::High => 16384,
            Res::VeryHigh => 32768,
            Res::Max => 65536,
        }
    }
    /// dynamic EQ is possible up to High (longer kernels can not follow the gain fast enough)
    pub fn dynamics_ok(self) -> bool {
        matches!(self, Res::Low | Res::Medium | Res::High)
    }
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Character {
    #[name = "Clean"]
    Clean,
    #[name = "Subtle"]
    Subtle,
    #[name = "Warm"]
    Warm,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum DispRange {
    #[name = "3 dB"]
    D3,
    #[name = "6 dB"]
    D6,
    #[name = "12 dB"]
    D12,
    #[name = "30 dB"]
    D30,
}

impl DispRange {
    pub fn db(self) -> f32 {
        match self {
            DispRange::D3 => 3.0,
            DispRange::D6 => 6.0,
            DispRange::D12 => 12.0,
            DispRange::D30 => 30.0,
        }
    }
}

macro_rules! simple_enum {
    ($name:ident { $($v:ident = $label:literal),* $(,)? }) => {
        #[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
        pub enum $name {
            $(#[name = $label] $v,)*
        }
    };
}

simple_enum!(AnRange { R60 = "60 dB", R90 = "90 dB", R120 = "120 dB" });
simple_enum!(AnRes { Low = "Low", Medium = "Medium", High = "High", Max = "Maximum" });
simple_enum!(AnSpeed { VSlow = "Very Slow", Slow = "Slow", Medium = "Medium", Fast = "Fast", VFast = "Very Fast" });
simple_enum!(AnTilt { T0 = "0 dB/oct", T15 = "1.5 dB/oct", T3 = "3 dB/oct", T45 = "4.5 dB/oct", T6 = "6 dB/oct" });

impl AnRange {
    pub fn db(self) -> f32 {
        match self {
            AnRange::R60 => 60.0,
            AnRange::R90 => 90.0,
            AnRange::R120 => 120.0,
        }
    }
}
impl AnRes {
    pub fn n(self) -> usize {
        match self {
            AnRes::Low => 2048,
            AnRes::Medium => 4096,
            AnRes::High => 8192,
            AnRes::Max => 16384,
        }
    }
}
impl AnSpeed {
    /// fall coefficient per GUI frame
    pub fn fall(self) -> f32 {
        match self {
            AnSpeed::VSlow => 0.025,
            AnSpeed::Slow => 0.06,
            AnSpeed::Medium => 0.12,
            AnSpeed::Fast => 0.25,
            AnSpeed::VFast => 0.5,
        }
    }
}
impl AnTilt {
    pub fn db(self) -> f32 {
        match self {
            AnTilt::T0 => 0.0,
            AnTilt::T15 => 1.5,
            AnTilt::T3 => 3.0,
            AnTilt::T45 => 4.5,
            AnTilt::T6 => 6.0,
        }
    }
}

pub fn fmt_hz(v: f32) -> String {
    if v >= 10000.0 {
        format!("{:.1}k", v / 1000.0)
    } else if v >= 1000.0 {
        format!("{:.2}k", v / 1000.0)
    } else if v >= 100.0 {
        format!("{:.0}", v)
    } else {
        format!("{:.1}", v)
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

fn db_fmt() -> Arc<dyn Fn(f32) -> String + Send + Sync> {
    Arc::new(|v| format!("{:+.1}", (v * 10.0).round() / 10.0 + 0.0))
}

fn freq(name: String, def: f32) -> FloatParam {
    FloatParam::new(name, def, FloatRange::Skewed { min: 10.0, max: 30000.0, factor: FloatRange::skew_factor(-2.2) })
        .with_smoother(SmoothingStyle::Logarithmic(20.0))
        .with_unit(" Hz")
        .with_value_to_string(Arc::new(fmt_hz))
        .with_string_to_value(Arc::new(parse_hz))
}

#[derive(Params)]
pub struct BandParams {
    /// the band exists (created in the display)
    #[id = "used"]
    pub used: BoolParam,
    #[id = "on"]
    pub on: BoolParam,
    #[id = "shape"]
    pub shape: EnumParam<Shape>,
    #[id = "slope"]
    pub slope: EnumParam<Slope>,
    #[id = "freq"]
    pub freq: FloatParam,
    #[id = "gain"]
    pub gain: FloatParam,
    #[id = "q"]
    pub q: FloatParam,
    #[id = "place"]
    pub place: EnumParam<Place>,
    #[id = "gq"]
    pub gq: BoolParam,
    /// dynamic range: 0 = a static band, < 0 compresses, > 0 expands
    #[id = "dyn"]
    pub dyn_range: FloatParam,
    /// dynamics active (off = "bypass dynamics", the range is kept)
    #[id = "dynon"]
    pub dyn_on: BoolParam,
    #[id = "thra"]
    pub thr_auto: BoolParam,
    #[id = "thr"]
    pub thr: FloatParam,
    #[id = "att"]
    pub attack: FloatParam,
    #[id = "rel"]
    pub release: FloatParam,
    #[id = "scfree"]
    pub sc_free: BoolParam,
    #[id = "sclo"]
    pub sc_lo: FloatParam,
    #[id = "schi"]
    pub sc_hi: FloatParam,
    #[id = "scext"]
    pub sc_ext: BoolParam,
    #[id = "spec"]
    pub spectral: BoolParam,
    #[id = "dens"]
    pub density: FloatParam,
    #[id = "sptilt"]
    pub sp_tilt: BoolParam,
}

impl BandParams {
    pub fn new(i: usize) -> Self {
        let n = |s: &str| format!("Band {} {}", i + 1, s);
        let pct = |name: String| {
            FloatParam::new(name, 50.0, FloatRange::Linear { min: 0.0, max: 100.0 })
                .with_step_size(1.0)
                .with_value_to_string(Arc::new(|v| if (v - 50.0).abs() < 0.5 { "auto".into() } else { format!("{:.0}%", v) }))
        };
        Self {
            used: BoolParam::new(n("Used"), false),
            on: BoolParam::new(n("Enabled"), true),
            shape: EnumParam::new(n("Shape"), Shape::Bell),
            slope: EnumParam::new(n("Slope"), Slope::S12),
            freq: freq(n("Frequency"), 1000.0),
            gain: FloatParam::new(n("Gain"), 0.0, FloatRange::Linear { min: -30.0, max: 30.0 })
                .with_step_size(0.01)
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_unit(" dB")
                .with_value_to_string(db_fmt()),
            q: FloatParam::new(n("Q"), 1.0, FloatRange::Skewed { min: 0.025, max: 40.0, factor: FloatRange::skew_factor(-2.0) })
                .with_smoother(SmoothingStyle::Logarithmic(20.0))
                .with_value_to_string(Arc::new(|v| if v < 1.0 { format!("{:.3}", v) } else { format!("{:.2}", v) })),
            place: EnumParam::new(n("Stereo Placement"), Place::Stereo),
            gq: BoolParam::new(n("Gain-Q Interaction"), false),
            dyn_range: FloatParam::new(n("Dynamic Range"), 0.0, FloatRange::Linear { min: -30.0, max: 30.0 })
                .with_step_size(0.01)
                .with_unit(" dB")
                .with_value_to_string(db_fmt()),
            dyn_on: BoolParam::new(n("Dynamics On"), true),
            thr_auto: BoolParam::new(n("Auto Threshold"), true),
            thr: FloatParam::new(n("Threshold"), -30.0, FloatRange::Linear { min: -80.0, max: 0.0 }).with_step_size(0.1).with_unit(" dB"),
            attack: pct(n("Attack")),
            release: pct(n("Release")),
            sc_free: BoolParam::new(n("Free Side Chain"), false),
            sc_lo: freq(n("Side Chain Low Cut"), 20.0),
            sc_hi: freq(n("Side Chain High Cut"), 20000.0),
            sc_ext: BoolParam::new(n("External Side Chain"), false),
            spectral: BoolParam::new(n("Spectral"), false),
            density: FloatParam::new(n("Spectral Density"), 50.0, FloatRange::Linear { min: 0.0, max: 100.0 }).with_step_size(1.0).with_unit("%"),
            sp_tilt: BoolParam::new(n("Spectral Tilt"), true),
        }
    }
}

#[derive(Params)]
pub struct EqParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    #[nested(array, group = "Band")]
    pub bands: Vec<BandParams>,

    #[id = "bypass"]
    pub bypass: BoolParam,
    #[id = "pmode"]
    pub pmode: EnumParam<PMode>,
    #[id = "res"]
    pub res: EnumParam<Res>,
    #[id = "char"]
    pub character: EnumParam<Character>,
    #[id = "autog"]
    pub auto_gain: BoolParam,
    #[id = "gscale"]
    pub gain_scale: FloatParam,
    #[id = "out"]
    pub out: FloatParam,
    #[id = "pan"]
    pub pan: FloatParam,
    #[id = "phase"]
    pub phase: BoolParam,

    // display settings (saved with the project, not automatable)
    #[id = "disp"]
    pub disp: EnumParam<DispRange>,
    #[id = "anpre"]
    pub an_pre: BoolParam,
    #[id = "anpost"]
    pub an_post: BoolParam,
    #[id = "ansc"]
    pub an_sc: BoolParam,
    #[id = "anrange"]
    pub an_range: EnumParam<AnRange>,
    #[id = "anres"]
    pub an_res: EnumParam<AnRes>,
    #[id = "anspeed"]
    pub an_speed: EnumParam<AnSpeed>,
    #[id = "antilt"]
    pub an_tilt: EnumParam<AnTilt>,
    #[id = "anfreeze"]
    pub an_freeze: BoolParam,
    #[id = "angrab"]
    pub an_grab: BoolParam,
    #[id = "ancoll"]
    pub an_coll: BoolParam,
    #[id = "piano"]
    pub piano: BoolParam,
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(crate::editor::BASE_W as u32, crate::editor::BASE_H as u32),
            bands: (0..NB).map(BandParams::new).collect(),
            bypass: BoolParam::new("Bypass", false).make_bypass(),
            pmode: EnumParam::new("Processing Mode", PMode::Zero),
            res: EnumParam::new("Linear Phase Resolution", Res::Medium),
            character: EnumParam::new("Character", Character::Clean),
            auto_gain: BoolParam::new("Auto Gain", false),
            gain_scale: FloatParam::new("Gain Scale", 100.0, FloatRange::Linear { min: -100.0, max: 200.0 })
                .with_step_size(1.0)
                .with_smoother(SmoothingStyle::Linear(30.0))
                .with_unit("%"),
            out: FloatParam::new("Output Gain", 0.0, FloatRange::Linear { min: -36.0, max: 36.0 })
                .with_step_size(0.1)
                .with_smoother(SmoothingStyle::Linear(25.0))
                .with_unit(" dB")
                .with_value_to_string(db_fmt()),
            pan: FloatParam::new("Output Pan", 0.0, FloatRange::Linear { min: -100.0, max: 100.0 })
                .with_step_size(1.0)
                .with_smoother(SmoothingStyle::Linear(25.0))
                .with_value_to_string(Arc::new(|v| {
                    if v.abs() < 0.5 {
                        "C".into()
                    } else if v < 0.0 {
                        format!("L{:.0}", -v)
                    } else {
                        format!("R{:.0}", v)
                    }
                })),
            phase: BoolParam::new("Phase Invert", false),
            disp: EnumParam::new("Display Range", DispRange::D12).non_automatable(),
            an_pre: BoolParam::new("Analyzer Pre", true).non_automatable(),
            an_post: BoolParam::new("Analyzer Post", true).non_automatable(),
            an_sc: BoolParam::new("Analyzer Side Chain", false).non_automatable(),
            an_range: EnumParam::new("Analyzer Range", AnRange::R90).non_automatable(),
            an_res: EnumParam::new("Analyzer Resolution", AnRes::High).non_automatable(),
            an_speed: EnumParam::new("Analyzer Speed", AnSpeed::Medium).non_automatable(),
            an_tilt: EnumParam::new("Analyzer Tilt", AnTilt::T45).non_automatable(),
            an_freeze: BoolParam::new("Analyzer Freeze", false).non_automatable(),
            an_grab: BoolParam::new("Spectrum Grab", true).non_automatable(),
            an_coll: BoolParam::new("Show Collisions", false).non_automatable(),
            piano: BoolParam::new("Piano Display", false).non_automatable(),
        }
    }
}
