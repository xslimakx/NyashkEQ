use nyashk_common::eq::*;
use nyashkeq::{Core, Mode};

const FS: f32 = 48000.0;

/// Steady-state gain in dB for a sine through the EQ core.
fn gain_db(cfg: &EqCfg, f: f32, mode: Mode, side_only: bool) -> f32 {
    let mut c = Core::new(FS);
    let (mut ei, mut eo) = (0.0f64, 0.0f64);
    for n in 0..48000 {
        let s = 0.1 * (2.0 * std::f32::consts::PI * f * n as f32 / FS).sin();
        let mut x = if side_only { [s, -s] } else { [s, s] };
        c.frame(cfg, true, mode, false, false, 1.0, &mut x, 2);
        if n > 24000 {
            ei += (s * s) as f64;
            eo += (x[0] * x[0]) as f64;
        }
    }
    10.0 * (eo / ei).log10() as f32
}

fn cfg() -> EqCfg {
    EqCfg::default()
}

#[test]
fn neutral_is_transparent() {
    for f in [50.0, 500.0, 5000.0, 15000.0] {
        assert!(gain_db(&cfg(), f, Mode::Stereo, false).abs() < 0.01);
    }
}

#[test]
fn bell_hits_the_requested_gain() {
    let mut c = cfg();
    c.lmf.freq = 1000.0;
    c.lmf.gain_db = 6.0;
    c.lmf.q = 1.0;
    assert!((gain_db(&c, 1000.0, Mode::Stereo, false) - 6.0).abs() < 0.15);
    assert!(gain_db(&c, 100.0, Mode::Stereo, false).abs() < 0.6);
    c.lmf.gain_db = -9.0;
    assert!((gain_db(&c, 1000.0, Mode::Stereo, false) + 9.0).abs() < 0.15);
}

#[test]
fn shelves_reach_their_gain() {
    let mut c = cfg();
    c.hf.gain_db = 8.0;
    c.hf.freq = 8000.0;
    assert!((gain_db(&c, 18000.0, Mode::Stereo, false) - 8.0).abs() < 1.0);
    assert!(gain_db(&c, 500.0, Mode::Stereo, false).abs() < 0.2);
    c.lf.gain_db = -6.0;
    c.lf.freq = 200.0;
    assert!((gain_db(&c, 40.0, Mode::Stereo, false) + 6.0).abs() < 1.0);
}

#[test]
fn highpass_is_third_order() {
    let mut c = cfg();
    c.hp_on = true;
    c.hp_f = 200.0;
    let at_fc = gain_db(&c, 200.0, Mode::Stereo, false);
    assert!((at_fc + 3.0).abs() < 0.6, "at cutoff {at_fc}");
    let octave = gain_db(&c, 100.0, Mode::Stereo, false);
    assert!((octave + 18.0).abs() < 3.0, "octave below {octave}");
    assert!(gain_db(&c, 2000.0, Mode::Stereo, false).abs() < 0.2);
}

#[test]
fn mid_side_modes_only_touch_their_part() {
    let mut c = cfg();
    c.lmf.freq = 1000.0;
    c.lmf.gain_db = 9.0;
    // a pure side signal is untouched in MID mode and boosted in SIDE mode
    assert!(gain_db(&c, 1000.0, Mode::Mid, true).abs() < 0.05);
    assert!((gain_db(&c, 1000.0, Mode::Side, true) - 9.0).abs() < 0.3);
    // a pure mid signal is boosted in MID mode and untouched in SIDE mode
    assert!((gain_db(&c, 1000.0, Mode::Mid, false) - 9.0).abs() < 0.3);
    assert!(gain_db(&c, 1000.0, Mode::Side, false).abs() < 0.05);
}

#[test]
fn graph_curve_matches_the_filter() {
    let mut c = cfg();
    c.hmf.freq = 3000.0;
    c.hmf.gain_db = -7.0;
    let co = EqCoefs::new(FS, &c);
    let measured = gain_db(&c, 3000.0, Mode::Stereo, false);
    assert!((co.response_db(FS, 3000.0) - measured).abs() < 0.1);
}

#[test]
fn brown_voicing_is_broader_at_low_gain() {
    let mut c = cfg();
    c.lmf.freq = 1000.0;
    c.lmf.gain_db = 4.0;
    c.voicing = Voicing::Black;
    let black = gain_db(&c, 1600.0, Mode::Stereo, false);
    c.voicing = Voicing::Brown;
    let brown = gain_db(&c, 1600.0, Mode::Stereo, false);
    assert!(brown > black + 0.3, "black {black} brown {brown}");
}
