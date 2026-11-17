//! Deeper behaviour checks of the EQ core (what a console-style parametric EQ should do).
use nyashk_common::eq::*;
use nyashkeq::{Core, Mode};
use std::f32::consts::PI;

fn gain_at(cfg: &EqCfg, f: f32, fs: f32) -> f32 {
    let mut c = Core::new(fs);
    let (mut ei, mut eo) = (0.0f64, 0.0f64);
    let n = (fs * 0.5) as usize;
    for i in 0..n {
        let s = 0.1 * (2.0 * PI * f * i as f32 / fs).sin();
        let mut x = [s, s];
        c.frame(cfg, true, Mode::Stereo, false, false, 1.0, &mut x, 2);
        if i > n / 2 {
            ei += (s * s) as f64;
            eo += (x[0] * x[0]) as f64;
        }
    }
    10.0 * (eo / ei).log10() as f32
}

#[test]
fn bell_bandwidth_follows_q() {
    // half-gain points of a +12 dB bell sit about one bandwidth (f / Q) apart
    for q in [0.5f32, 1.0, 2.0, 4.0] {
        let mut c = EqCfg::default();
        c.lmf = Band { freq: 1000.0, gain_db: 12.0, q, bell: true };
        let mut lo = 0.0;
        let mut hi = 0.0;
        let mut f = 100.0f32;
        while f < 10000.0 {
            let g = gain_at(&c, f, 48000.0);
            if f < 1000.0 && g >= 6.0 && lo == 0.0 {
                lo = f;
            }
            if f > 1000.0 && g < 6.0 && hi == 0.0 {
                hi = f;
            }
            f *= 1.02;
        }
        let oct = (hi / lo).log2();
        println!("Q {q}: half-gain width {lo:.0}-{hi:.0} Hz = {oct:.2} oct");
        // RBJ peaking with Q: bandwidth in octaves ~ 2 asinh(1 / (2Q)) / ln 2 (measured at the half-gain points)
        let expect = 2.0 * (1.0 / (2.0 * q)).asinh() / std::f32::consts::LN_2;
        assert!((oct - expect).abs() < 0.25 * expect + 0.05, "Q {q}: {oct:.2} oct, expected ~{expect:.2}");
    }
}

#[test]
fn high_bands_are_not_cramped_near_nyquist() {
    // a +12 dB bell at 12 kHz must reach its gain at 12 kHz at 44.1 kHz too
    for fs in [44100.0f32, 48000.0, 96000.0] {
        let mut c = EqCfg::default();
        c.hmf = Band { freq: 12000.0, gain_db: 12.0, q: 1.0, bell: true };
        let g = gain_at(&c, 12000.0, fs);
        let g16 = gain_at(&c, 16000.0, fs);
        println!("fs {fs}: 12 kHz {g:.2} dB, 16 kHz {g16:.2} dB");
        assert!((g - 12.0).abs() < 0.5, "fs {fs}: {g}");
    }
}

#[test]
fn lowpass_and_highpass_corner_and_slope() {
    let mut c = EqCfg::default();
    c.lp_on = true;
    c.lp_f = 5000.0;
    let at = |c: &EqCfg, f| gain_at(c, f, 48000.0);
    let (g0, g1) = (at(&c, 5000.0), at(&c, 10000.0));
    println!("LP 5k: at corner {g0:.2} dB, one octave up {g1:.2} dB");
    assert!((g0 + 3.0).abs() < 0.6, "LP corner {g0}");
    assert!(g1 < -10.0, "LP slope {g1}");
    let mut c = EqCfg::default();
    c.hp_on = true;
    c.hp_f = 100.0;
    let (h0, h1) = (at(&c, 100.0), at(&c, 50.0));
    println!("HP 100: at corner {h0:.2} dB, one octave down {h1:.2} dB");
    assert!(h0 < -1.0 && h0 > -6.5, "HP corner {h0}");
    assert!(h1 < -14.0, "HP slope {h1}");
}

#[test]
fn analog_is_quiet_at_low_level_and_mild_when_hot() {
    let fs = 48000.0;
    let thd = |amp: f32| {
        let mut c = Core::new(fs);
        let cfg = EqCfg::default();
        let n = 48000;
        let f = 1000.0;
        let mut y = vec![];
        for i in 0..n {
            let s = amp * (2.0 * PI * f * i as f32 / fs).sin();
            let mut x = [s, s];
            c.frame(&cfg, true, Mode::Stereo, false, true, 1.0, &mut x, 2);
            y.push(x[0]);
        }
        let tail = &y[24000..];
        let h = |k: f32| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, v) in tail.iter().enumerate() {
                let ph = 2.0 * std::f64::consts::PI * (k * f) as f64 * (i + 24000) as f64 / fs as f64;
                re += *v as f64 * ph.cos();
                im += *v as f64 * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        let fund = h(1.0);
        let harm = (2..8).map(|k| h(k as f32).powi(2)).sum::<f64>().sqrt();
        (harm / fund * 100.0) as f32
    };
    let (lo, hi) = (thd(0.01), thd(0.7));
    println!("analog THD: -40 dBFS {lo:.4} %, -3 dBFS {hi:.3} %");
    assert!(lo < 0.05, "analog adds distortion at low level: {lo}");
    assert!(hi > 0.05 && hi < 3.0, "analog at -3 dBFS: {hi}");
}

#[test]
fn fast_automation_does_not_click() {
    // the plugin refreshes the coefficients every 16 samples from smoothed values: sweep the gain
    // of a bell through its range in 50 ms over a low tone and look for jumps
    let fs = 48000.0;
    let mut c = Core::new(fs);
    let mut cfg = EqCfg::default();
    let mut prev = 0.0f32;
    let mut worst = 0.0f32;
    let f = 100.0;
    for i in 0..(fs * 1.0) as usize {
        let t = i as f32 / fs;
        if i % 16 == 0 {
            let u = ((t - 0.3) / 0.05).clamp(0.0, 1.0);
            cfg.lf = Band { freq: 100.0, gain_db: -15.0 + 30.0 * u, q: 1.0, bell: true };
            cfg.lmf = Band { freq: 200.0 + 3000.0 * u, gain_db: 12.0, q: 2.0, bell: true };
        }
        let s = 0.05 * (2.0 * PI * f * t).sin();
        let mut x = [s, s];
        c.frame(&cfg, true, Mode::Stereo, false, false, 1.0, &mut x, 2);
        if i > 100 {
            worst = worst.max((x[0] - prev).abs());
        }
        prev = x[0];
    }
    // the tone at up to +15 dB moves at most 0.28 * 2 pi * 100 / fs per sample
    let slope = 0.05 * 5.62 * 2.0 * PI * f / fs;
    println!("largest step {worst:.5} vs tone slope {slope:.5}");
    assert!(worst < slope * 1.5, "automation click: {worst}");
}

#[test]
fn switches_do_not_click() {
    // EQ IN, high-pass, low-pass and bypass toggled while a boosted low tone plays
    let fs = 48000.0;
    let f = 80.0;
    for what in ["eq_in", "hp", "lp", "bypass"] {
        let mut c = Core::new(fs);
        let mut cfg = EqCfg::default();
        cfg.lf = Band { freq: 80.0, gain_db: 12.0, q: 1.0, bell: true };
        cfg.hp_f = 120.0;
        cfg.lp_f = 2000.0;
        let mut eq_in = true;
        let mut prev = 0.0f32;
        let mut worst = 0.0f32;
        for i in 0..(fs * 1.0) as usize {
            let t = i as f32 / fs;
            if i % 12000 == 0 && i > 0 {
                match what {
                    "eq_in" => eq_in = !eq_in,
                    "hp" => cfg.hp_on = !cfg.hp_on,
                    "lp" => cfg.lp_on = !cfg.lp_on,
                    _ => c.bypass = !c.bypass,
                }
            }
            // a mix of 80 Hz and 1 kHz, so the low-pass has something to cut
            let s = 0.05 * (2.0 * PI * f * t).sin() + 0.02 * (2.0 * PI * 1000.0 * t).sin();
            let mut x = [s, s];
            c.frame(&cfg, eq_in, Mode::Stereo, false, false, 1.0, &mut x, 2);
            if i > 2000 {
                worst = worst.max((x[0] - prev).abs());
            }
            prev = x[0];
        }
        let slope = (0.05 * 4.0 * 2.0 * PI * f + 0.02 * 4.0 * 2.0 * PI * 1000.0) / fs;
        println!("{what}: largest step {worst:.5} (signal slope up to {slope:.5})");
        assert!(worst < slope * 1.2, "{what} clicks: {worst}");
    }
}

#[test]
fn switches_leave_no_sharp_corners() {
    // second difference of the output relative to the same run without switching
    let fs = 48000.0;
    for what in ["eq_in", "hp", "lp", "bypass", "analog"] {
        let run = |toggle: bool| {
            let mut c = Core::new(fs);
            let mut cfg = EqCfg::default();
            cfg.lf = Band { freq: 80.0, gain_db: 12.0, q: 1.0, bell: true };
            cfg.hp_f = 120.0;
            cfg.lp_f = 2000.0;
            let (mut eq_in, mut analog) = (true, false);
            let (mut y1, mut y2, mut worst) = (0.0f32, 0.0f32, 0.0f32);
            for i in 0..(fs * 1.0) as usize {
                if toggle && i % 12000 == 0 && i > 0 {
                    match what {
                        "eq_in" => eq_in = !eq_in,
                        "hp" => cfg.hp_on = !cfg.hp_on,
                        "lp" => cfg.lp_on = !cfg.lp_on,
                        "analog" => analog = !analog,
                        _ => c.bypass = !c.bypass,
                    }
                }
                let t = i as f32 / fs;
                let s = 0.05 * (2.0 * PI * 80.0 * t).sin() + 0.02 * (2.0 * PI * 700.0 * t).sin();
                let mut x = [s, s];
                c.frame(&cfg, eq_in, Mode::Stereo, false, analog, 1.0, &mut x, 2);
                if i > 4800 {
                    worst = worst.max((x[0] - 2.0 * y1 + y2).abs());
                }
                y2 = y1;
                y1 = x[0];
            }
            worst
        };
        let r = run(true) / run(false);
        println!("{what}: sharpest corner x{r:.2}");
        assert!(r < 2.0, "{what} clicks (x{r:.1})");
    }
}
