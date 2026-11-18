//! Robustness: every sample rate, extreme settings, all modes, bad input from the host.
use nyashk_common::eq::*;
use nyashkeq::{Core, Mode};

fn extreme(bell: bool) -> EqCfg {
    EqCfg {
        hp_on: true,
        hp_f: 400.0,
        lp_on: true,
        lp_f: 3000.0,
        lf: Band { freq: 30.0, gain_db: 20.0, q: 1.0, bell },
        lmf: Band { freq: 200.0, gain_db: -20.0, q: 12.0, bell: true },
        hmf: Band { freq: 20000.0, gain_db: 20.0, q: 12.0, bell: true },
        hf: Band { freq: 22000.0, gain_db: 20.0, q: 1.0, bell },
        voicing: Voicing::Brown,
    }
}

#[test]
fn every_rate_mode_and_extreme_setting_stays_finite() {
    for &fs in &[8000.0f32, 22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
        for cfg in [EqCfg::default(), extreme(false), extreme(true)] {
            for mode in [Mode::Stereo, Mode::Mid, Mode::Side] {
                for analog in [false, true] {
                    let mut c = Core::new(fs);
                    let mut r = 7u32;
                    for n in 0..(fs as usize / 2) {
                        r = r.wrapping_mul(1664525).wrapping_add(1013904223);
                        let w = (r >> 8) as f32 / 8388608.0 - 1.0;
                        let v = match (n / 3000) % 3 { 0 => w, 1 => 0.0, _ => if (n / 7) % 2 == 0 { 1.0 } else { -1.0 } };
                        let mut x = [v, -0.6 * v];
                        c.frame(&cfg, true, mode, n % 5000 < 2500, analog, 4.0, &mut x, 2);
                        assert!(x[0].is_finite() && x[1].is_finite() && x[0].abs() < 1e4 && x[1].abs() < 1e4, "fs {fs} {mode:?} n {n}: {x:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn recovers_after_nan_or_inf_from_the_host() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut c = Core::new(48000.0);
        let cfg = extreme(true);
        for n in 0..48000 {
            let v = if (100..110).contains(&n) { bad } else { 0.3 * (n as f32 * 0.05).sin() };
            let mut x = [v, v];
            c.frame(&cfg, true, Mode::Stereo, false, true, 1.0, &mut x, 2);
            if n > 2000 {
                assert!(x[0].is_finite() && x[1].is_finite(), "{bad}: still broken at n {n}");
            }
        }
    }
}
