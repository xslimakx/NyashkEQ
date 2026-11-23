//! NyashkEQ 2: 24-band dynamic equalizer with Zero Latency, Natural Phase and Linear Phase
//! processing, ten band shapes with slopes up to 96 dB/oct and brickwall, per-band stereo / mid /
//! side placement, external side chain, character modes, auto gain and a live analyzer.

use nih_plug::prelude::*;
use nyashk_common::tap::Tap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;

pub mod conv;
mod editor;
pub mod engine;
pub mod filters;
pub mod params;
pub mod spectral;

use conv::NB;
use engine::{BandIn, Engine, Settings};
use params::EqParams;
pub use params::{fmt_hz, parse_hz};

pub struct Shared {
    pub tap_pre: Tap,
    pub tap_post: Tap,
    pub tap_sc: Tap,
    pub out_db: [AtomicU32; 2],
    pub fs: AtomicU32,
    pub latency: AtomicU32,
    pub dyn_db: [AtomicU32; NB],
    pub lvl_db: [AtomicU32; NB],
    pub thr_db: [AtomicU32; NB],
    pub solo: AtomicI32,
    pub dyn_blocked: AtomicBool,
    pub has_sc: AtomicBool,
    /// spectral dynamics: total gain per FFT bin (dB) and the FFT length
    pub sp_show: Vec<AtomicU32>,
    pub sp_n: AtomicU32,
    pub sp_on: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        let z = || AtomicU32::new(0f32.to_bits());
        Self {
            tap_pre: Tap::new(16384),
            tap_post: Tap::new(16384),
            tap_sc: Tap::new(16384),
            out_db: [AtomicU32::new((-90f32).to_bits()), AtomicU32::new((-90f32).to_bits())],
            fs: AtomicU32::new(48000f32.to_bits()),
            latency: AtomicU32::new(0),
            dyn_db: std::array::from_fn(|_| z()),
            lvl_db: std::array::from_fn(|_| AtomicU32::new((-120f32).to_bits())),
            thr_db: std::array::from_fn(|_| AtomicU32::new((-30f32).to_bits())),
            solo: AtomicI32::new(-1),
            dyn_blocked: AtomicBool::new(false),
            has_sc: AtomicBool::new(false),
            sp_show: (0..8200).map(|_| AtomicU32::new(0)).collect(),
            sp_n: AtomicU32::new(2048),
            sp_on: AtomicBool::new(false),
        }
    }
    pub fn get(a: &AtomicU32) -> f32 {
        f32::from_bits(a.load(Ordering::Relaxed))
    }
}

/// Settings for the engine from the parameters. `step`: samples to advance the smoothers by.
pub fn settings(p: &EqParams, step: u32, solo: i32) -> Settings {
    let mut s = Settings::default();
    for (i, bp) in p.bands.iter().enumerate() {
        let (f, g, q) = if step > 0 { (bp.freq.smoothed.next_step(step), bp.gain.smoothed.next_step(step), bp.q.smoothed.next_step(step)) } else { (bp.freq.value(), bp.gain.value(), bp.q.value()) };
        s.bands[i] = BandIn {
            used: bp.used.value(),
            on: bp.on.value(),
            spec: filters::Spec { shape: bp.shape.value(), slope: bp.slope.value(), freq: f as f64, gain_db: g as f64, q: q as f64, gq: bp.gq.value() },
            place: bp.place.value(),
            dyn_range: bp.dyn_range.value() as f64,
            dyn_on: bp.dyn_on.value(),
            thr_auto: bp.thr_auto.value(),
            thr: bp.thr.value() as f64,
            att: bp.attack.value() as f64 / 100.0,
            rel: bp.release.value() as f64 / 100.0,
            sc_free: bp.sc_free.value(),
            sc_lo: bp.sc_lo.value() as f64,
            sc_hi: bp.sc_hi.value() as f64,
            sc_ext: bp.sc_ext.value(),
            spectral: bp.spectral.value(),
            density: bp.density.value() as f64 / 100.0,
            sp_tilt: bp.sp_tilt.value(),
        };
    }
    s.pmode = p.pmode.value();
    s.res = p.res.value();
    s.character = p.character.value();
    s.auto_gain = p.auto_gain.value();
    s.gain_scale = if step > 0 { p.gain_scale.smoothed.next_step(step) } else { p.gain_scale.value() } as f64 / 100.0;
    s.out_db = if step > 0 { p.out.smoothed.next_step(step) } else { p.out.value() };
    s.pan = if step > 0 { p.pan.smoothed.next_step(step) } else { p.pan.value() } / 100.0;
    s.phase = p.phase.value();
    s.bypass = p.bypass.value();
    s.solo = solo;
    s
}

pub struct NyashkEQ {
    params: Arc<EqParams>,
    shared: Arc<Shared>,
    engine: Option<Engine>,
    cur_latency: u32,
}

impl Default for NyashkEQ {
    fn default() -> Self {
        Self { params: Arc::new(EqParams::default()), shared: Arc::new(Shared::new()), engine: None, cur_latency: 0 }
    }
}

impl Plugin for NyashkEQ {
    const NAME: &'static str = "NyashkEQ";
    const VENDOR: &'static str = "Nyashk";
    const URL: &'static str = "https://github.com/xslimakx";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[new_nonzero_u32(2)],
            names: PortNames { aux_inputs: &["Sidechain"], ..PortNames::const_default() },
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            aux_input_ports: &[new_nonzero_u32(1)],
            names: PortNames { aux_inputs: &["Sidechain"], ..PortNames::const_default() },
            ..AudioIOLayout::const_default()
        },
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

    fn initialize(&mut self, layout: &AudioIOLayout, config: &BufferConfig, context: &mut impl InitContext<Self>) -> bool {
        let nch = layout.main_output_channels.map(|n| n.get() as usize).unwrap_or(2);
        let mut e = Engine::new(config.sample_rate as f64, nch);
        e.set_offline(config.process_mode == ProcessMode::Offline);
        e.prime(&settings(&self.params, 0, -1));
        self.cur_latency = e.latency() as u32;
        context.set_latency_samples(self.cur_latency);
        self.shared.latency.store(self.cur_latency, Ordering::Relaxed);
        self.shared.fs.store(config.sample_rate.to_bits(), Ordering::Relaxed);
        self.engine = Some(e);
        true
    }

    fn reset(&mut self) {
        if let Some(e) = self.engine.as_mut() {
            e.reset();
        }
    }

    fn process(&mut self, buffer: &mut Buffer, aux: &mut AuxiliaryBuffers, context: &mut impl ProcessContext<Self>) -> ProcessStatus {
        nyashk_common::demo::sanitize(buffer);
        #[cfg(feature = "standalone")]
        nyashk_common::demo::fill(buffer, 48000.0);
        let Some(e) = self.engine.as_mut() else { return ProcessStatus::Normal };
        let p = &self.params;
        let solo = self.shared.solo.load(Ordering::Relaxed);
        let nch = buffer.channels().min(2);
        let n = buffer.samples();
        let sc_buf = aux.inputs.get_mut(0).map(|b| b.as_slice_immutable());
        let sc_ch = sc_buf.as_ref().map(|b| b.len()).unwrap_or(0);
        let mut has_sc = false;
        let out = buffer.as_slice();
        let mut pk = [0.0f32; 2];
        let want_sc_tap = p.an_sc.value();
        let mut i = 0;
        while i < n {
            let len = (n - i).min(16);
            e.set(&settings(p, len as u32, solo));
            for k in i..i + len {
                let x = [out[0][k], if nch > 1 { out[1][k] } else { 0.0 }];
                let sc = match &sc_buf {
                    Some(b) if sc_ch > 0 => {
                        let a = b[0][k];
                        let c = if sc_ch > 1 { b[1][k] } else { a };
                        if a != 0.0 || c != 0.0 {
                            has_sc = true;
                        }
                        [a, c]
                    }
                    _ => [0.0, 0.0],
                };
                self.shared.tap_pre.push(if nch > 1 { 0.5 * (x[0] + x[1]) } else { x[0] });
                if want_sc_tap {
                    self.shared.tap_sc.push(0.5 * (sc[0] + sc[1]));
                }
                let y = e.process(x, sc);
                out[0][k] = y[0];
                pk[0] = pk[0].max(y[0].abs());
                if nch > 1 {
                    out[1][k] = y[1];
                    pk[1] = pk[1].max(y[1].abs());
                }
                self.shared.tap_post.push(if nch > 1 { 0.5 * (y[0] + y[1]) } else { y[0] });
            }
            i += len;
        }
        if nch == 1 {
            pk[1] = pk[0];
        }
        for c in 0..2 {
            self.shared.out_db[c].store((20.0 * (pk[c] + 1e-9).log10()).to_bits(), Ordering::Relaxed);
        }
        for b in 0..NB {
            self.shared.dyn_db[b].store(e.dyn_out[b].to_bits(), Ordering::Relaxed);
            self.shared.lvl_db[b].store(e.lvl_out[b].to_bits(), Ordering::Relaxed);
            self.shared.thr_db[b].store(e.thr_out[b].to_bits(), Ordering::Relaxed);
        }
        self.shared.dyn_blocked.store(e.dyn_blocked, Ordering::Relaxed);
        self.shared.has_sc.store(has_sc, Ordering::Relaxed);
        let sp_on = e.spectral_running();
        self.shared.sp_on.store(sp_on, Ordering::Relaxed);
        if sp_on {
            self.shared.sp_n.store(e.spec.n as u32, Ordering::Relaxed);
            for (a, v) in self.shared.sp_show.iter().zip(e.spec.show.iter()) {
                a.store(v.to_bits(), Ordering::Relaxed);
            }
        }
        let lat = e.latency() as u32;
        if lat != self.cur_latency {
            self.cur_latency = lat;
            context.set_latency_samples(lat);
            self.shared.latency.store(lat, Ordering::Relaxed);
        }
        ProcessStatus::Normal
    }
}

impl Vst3Plugin for NyashkEQ {
    const VST3_CLASS_ID: [u8; 16] = *b"NyashkEQVST3!!!!";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx, Vst3SubCategory::Eq];
}

nih_export_vst3!(NyashkEQ);
