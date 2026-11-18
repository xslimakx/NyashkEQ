//! The plain path (all bands at 0 dB) must not colour the top end at any sample rate.
use nyashk_common::eq::*;
use nyashkeq::{Core, Mode};
#[test]
fn flat_settings_are_flat_to_20k_at_every_rate() {
    for analog in [false, true] {
    for fs in [44100.0f32, 48000.0, 96000.0] {
        for f in [10000.0f32, 15000.0, 17000.0, 18000.0, 19000.0, 20000.0] {
            let mut c = Core::new(fs);
            let (mut ei, mut eo) = (0.0f64, 0.0f64);
            let n = (fs * 0.3) as usize;
            for i in 0..n {
                let s = 0.1 * (2.0 * std::f32::consts::PI * f * i as f32 / fs).sin();
                let mut x = [s, s];
                c.frame(&EqCfg::default(), true, Mode::Stereo, false, analog, 1.0, &mut x, 2);
                if i > n / 2 { ei += (s * s) as f64; eo += (x[0] * x[0]) as f64; }
            }
            let g = 10.0 * (eo / ei).log10();
            print!("{fs} {f}: {g:.2}  ");
            assert!(g.abs() < 0.1, "analog {analog} fs {fs} f {f}: {g} dB");
        }
        println!();
    }
    }
}
