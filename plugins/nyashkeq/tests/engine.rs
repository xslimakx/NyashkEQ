//! Engine behaviour: every mode lands on the analog curve, latency is what is reported, mid / side
//! placement, dynamics, side chain, switching without clicks, fuzzing.
use nyashkeq::conv::Place;
use nyashkeq::engine::{BandIn, Engine, Settings};
use nyashkeq::filters::{analog, Shape, Slope, Spec};
use nyashkeq::params::{Character, PMode, Res};
use std::f64::consts::PI;

const FS: f64 = 48000.0;

fn band(shape: Shape, slope: Slope, f: f64, g: f64, q: f64) -> BandIn {
    BandIn { used: true, spec: Spec { shape, slope, freq: f, gain_db: g, q, gq: false }, ..Default::default() }
}

fn settings(mode: PMode, bands: &[BandIn]) -> Settings {
    let mut s = Settings { pmode: mode, ..Default::default() };
    for (i, b) in bands.iter().enumerate() {
        s.bands[i] = *b;
    }
    s
}

fn engine(s: &Settings, nch: usize) -> Engine {
    let mut e = Engine::new(FS, nch);
    e.set_offline(true);
    e.prime(s);
    e.set(s);
    e
}

/// gain (dB) of a sine at f through the engine, left / right inputs scaled by (a, b)
fn sine_gain(e: &mut Engine, f: f64, a: f32, b: f32) -> (f64, f64) {
    let n = (FS * 0.6) as usize + e.latency();
    let (mut ei, mut eo) = (0.0f64, [0.0f64; 2]);
    let start = n - (FS * 0.2) as usize;
    for i in 0..n {
        let x = (2.0 * PI * f * i as f64 / FS).sin() as f32;
        let y = e.process([x * a, x * b], [0.0, 0.0]);
        if i >= start {
            ei += (x as f64).powi(2);
            eo[0] += (y[0] as f64).powi(2);
            eo[1] += (y[1] as f64).powi(2);
        }
    }
    let ga = if a != 0.0 { 10.0 * (eo[0] / (ei * (a as f64).powi(2))).log10() } else { 10.0 * (eo[0] / ei + 1e-30).log10() };
    let gb = if b != 0.0 { 10.0 * (eo[1] / (ei * (b as f64).powi(2))).log10() } else { 10.0 * (eo[1] / ei + 1e-30).log10() };
    (ga, gb)
}

#[test]
fn every_mode_follows_the_analog_curve() {
    let bands = [band(Shape::Bell, Slope::S12, 1000.0, 9.0, 1.4), band(Shape::LowCut, Slope::S24, 60.0, 0.0, 1.0), band(Shape::HighShelf, Slope::S12, 14000.0, -6.0, 1.0), band(Shape::Bell, Slope::S24, 250.0, -5.0, 3.0)];
    let total = |f: f64| bands.iter().map(|b| analog(&b.spec).db(f)).sum::<f64>();
    for mode in [PMode::Zero, PMode::Natural, PMode::Linear] {
        let s = settings(mode, &bands);
        for f in [40.0, 120.0, 250.0, 1000.0, 3000.0, 12000.0, 18000.0] {
            let mut e = engine(&s, 2);
            let (gl, gr) = sine_gain(&mut e, f, 1.0, 1.0);
            let want = total(f);
            let tol = match mode {
                PMode::Zero => 0.4,
                // linear phase at 4 k... uses Medium resolution: 40 Hz is at its limit
                PMode::Linear if f < 100.0 => 1.0,
                _ => 0.25,
            };
            assert!((gl - want).abs() < tol && (gr - want).abs() < tol, "{mode:?} at {f} Hz: {gl:.2} / {gr:.2} dB, analog {want:.2}");
        }
    }
}

#[test]
fn latency_is_where_the_impulse_is() {
    for (mode, res) in [(PMode::Zero, Res::Low), (PMode::Natural, Res::Low), (PMode::Linear, Res::Low), (PMode::Linear, Res::High), (PMode::Linear, Res::Max)] {
        let mut s = settings(mode, &[band(Shape::Bell, Slope::S12, 3000.0, 4.0, 1.0)]);
        s.res = res;
        let mut e = engine(&s, 2);
        let lat = e.latency();
        // let the first kernel arrive
        for _ in 0..(FS as usize / 5) {
            e.process([0.0, 0.0], [0.0, 0.0]);
        }
        let n = lat + 4096;
        let mut ir = vec![0.0f32; n];
        for (i, v) in ir.iter_mut().enumerate() {
            *v = e.process([if i == 0 { 1.0 } else { 0.0 }; 2], [0.0, 0.0])[0];
        }
        let peak = ir.iter().enumerate().fold((0, 0.0f32), |a, (i, &v)| if v.abs() > a.1 { (i, v.abs()) } else { a }).0;
        eprintln!("{mode:?} {res:?}: latency {lat} ({:.1} ms), impulse peak at {peak}", lat as f64 / FS * 1000.0);
        assert!((peak as i64 - lat as i64).abs() <= 2, "{mode:?}: peak {peak} latency {lat}");
        if mode == PMode::Linear {
            // linear phase: symmetric around the peak
            let asym: f32 = (1..400).map(|k| (ir[peak + k] - ir[peak - k]).abs()).sum();
            let tot: f32 = (1..400).map(|k| ir[peak + k].abs()).sum();
            assert!(asym < 0.02 * tot.max(1e-6), "not symmetric: {asym} vs {tot}");
        }
    }
}

#[test]
fn mid_side_and_left_right_placement() {
    for mode in [PMode::Zero, PMode::Natural, PMode::Linear] {
        let mut b = band(Shape::Bell, Slope::S12, 1000.0, 10.0, 1.0);
        b.place = Place::Mid;
        let s = settings(mode, &[b]);
        // mid signal (L = R) is boosted, side signal (L = -R) is not
        let mut e = engine(&s, 2);
        let (m, _) = sine_gain(&mut e, 1000.0, 1.0, 1.0);
        let mut e = engine(&s, 2);
        let (sd, _) = sine_gain(&mut e, 1000.0, 1.0, -1.0);
        assert!((m - 10.0).abs() < 0.3 && sd.abs() < 0.3, "{mode:?} mid band: mid {m:.2}, side {sd:.2}");
        b.place = Place::Right;
        let s = settings(mode, &[b]);
        let mut e = engine(&s, 2);
        let (l, r) = sine_gain(&mut e, 1000.0, 1.0, 1.0);
        assert!(l.abs() < 0.3 && (r - 10.0).abs() < 0.3, "{mode:?} right band: {l:.2} {r:.2}");
    }
}

#[test]
fn dynamic_band_limits_loud_signal_only() {
    for mode in [PMode::Zero, PMode::Linear] {
        let mut b = band(Shape::Bell, Slope::S12, 1000.0, 0.0, 1.0);
        b.dyn_range = -12.0;
        b.thr_auto = false;
        b.thr = -24.0;
        let s = settings(mode, &[b]);
        let mut e = engine(&s, 2);
        let (loud, _) = sine_gain(&mut e, 1000.0, 0.5, 0.5); // about -6 dB, 18 over
        let mut e = engine(&s, 2);
        let (quiet, _) = sine_gain(&mut e, 1000.0, 0.01, 0.01); // -40 dB, under
        let mut e = engine(&s, 2);
        let (away, _) = sine_gain(&mut e, 8000.0, 0.5, 0.5); // outside the band
        eprintln!("{mode:?}: loud {loud:.2} quiet {quiet:.2} away {away:.2}");
        assert!(loud < -9.0 && loud > -12.5, "{mode:?} loud {loud}");
        assert!(quiet.abs() < 0.3, "{mode:?} quiet {quiet}");
        assert!(away.abs() < 1.0, "{mode:?} away {away}");
        // expansion
        let mut b2 = b;
        b2.dyn_range = 6.0;
        let s = settings(mode, &[b2]);
        let mut e = engine(&s, 2);
        let (up, _) = sine_gain(&mut e, 1000.0, 0.5, 0.5);
        assert!(up > 5.0 && up < 6.5, "{mode:?} expand {up}");
    }
    // external side chain: the band ducks when the side chain is loud, not the input
    let mut b = band(Shape::Bell, Slope::S12, 1000.0, 0.0, 1.0);
    b.dyn_range = -10.0;
    b.thr_auto = false;
    b.thr = -30.0;
    b.sc_ext = true;
    let s = settings(PMode::Zero, &[b]);
    for (sc_amp, want_lo, want_hi) in [(0.0f32, -0.3, 0.3), (0.5, -10.5, -8.0)] {
        let mut e = engine(&s, 2);
        let (mut ei, mut eo) = (0.0, 0.0);
        for i in 0..(FS as usize) {
            let x = (2.0 * PI * 1000.0 * i as f64 / FS).sin() as f32 * 0.5;
            let y = e.process([x, x], [x / 0.5 * sc_amp, x / 0.5 * sc_amp]);
            if i > FS as usize / 2 {
                ei += (x as f64).powi(2);
                eo += (y[0] as f64).powi(2);
            }
        }
        let g = 10.0 * (eo / ei).log10();
        assert!(g > want_lo && g < want_hi, "side chain {sc_amp}: {g}");
    }
}

#[test]
fn neutral_is_transparent_and_switching_is_smooth() {
    // no bands, zero latency: bit-exact
    let s = settings(PMode::Zero, &[]);
    let mut e = engine(&s, 2);
    for i in 0..5000 {
        let x = ((i * 7919 % 1000) as f32 / 500.0) - 1.0;
        let y = e.process([x, -x], [0.0, 0.0]);
        assert!(y[0] == x && y[1] == -x);
    }
    // switching modes / shapes while a tone plays: no jumps bigger than the signal's own step
    let mut s = settings(PMode::Zero, &[band(Shape::Bell, Slope::S12, 1000.0, 6.0, 1.0)]);
    let mut e = engine(&s, 2);
    let mut prev = 0.0f32;
    let mut worst = 0.0f32;
    let n = (FS * 4.0) as usize;
    for i in 0..n {
        let t = i as f64 / FS;
        if i == n / 8 {
            s.bands[0].spec.shape = Shape::HighShelf;
        }
        if i == n / 4 {
            s.pmode = PMode::Natural;
        }
        if i == n / 2 {
            s.pmode = PMode::Linear;
        }
        if i == 3 * n / 4 {
            s.bands[0].spec.slope = Slope::S48;
            s.bands[0].place = Place::Side;
        }
        if i == 7 * n / 8 {
            s.bands[0].used = false;
            s.character = Character::Warm;
        }
        e.set(&s);
        let x = (2.0 * PI * 220.0 * t).sin() as f32 * 0.5;
        let y = e.process([x, x * 0.8], [0.0, 0.0])[0];
        assert!(y.is_finite());
        if i > 10 {
            worst = worst.max((y - prev).abs());
        }
        prev = y;
    }
    // a 220 Hz sine at 0.5 (x 2 with the boost) steps at most ~0.03 per sample
    assert!(worst < 0.08, "jump {worst}");
}

#[test]
fn fuzz_every_shape_slope_and_mode() {
    let mut seed = 12345u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % 1_000_000) as f64 / 1_000_000.0
    };
    for mode in [PMode::Zero, PMode::Natural, PMode::Linear] {
        for fs in [44100.0, 96000.0] {
            let mut e = Engine::new(fs, 2);
            e.set_offline(true);
            let mut s = settings(mode, &[]);
            e.prime(&s);
            for blk in 0..300 {
                if blk % 10 == 0 {
                    for b in s.bands.iter_mut().take(12) {
                        b.used = rnd() < 0.7;
                        b.spec.shape = Shape::ALL[(rnd() * 10.0) as usize % 10];
                        b.spec.slope = Slope::ALL[(rnd() * 11.0) as usize % 11];
                        b.spec.freq = 10.0 * 3000f64.powf(rnd());
                        b.spec.gain_db = rnd() * 60.0 - 30.0;
                        b.spec.q = 0.025 * 1600f64.powf(rnd());
                        b.place = [Place::Stereo, Place::Left, Place::Right, Place::Mid, Place::Side][(rnd() * 5.0) as usize % 5];
                        b.dyn_range = if rnd() < 0.4 { rnd() * 60.0 - 30.0 } else { 0.0 };
                        b.thr_auto = rnd() < 0.5;
                        b.thr = -rnd() * 60.0;
                    }
                    s.gain_scale = rnd() * 3.0 - 1.0;
                    s.auto_gain = rnd() < 0.5;
                    s.res = [Res::Low, Res::Medium, Res::High][(rnd() * 3.0) as usize % 3];
                }
                e.set(&s);
                for i in 0..256 {
                    let x = if (blk * 256 + i) % 7777 == 0 { f32::NAN } else { ((rnd() * 2.0 - 1.0) * 0.5) as f32 };
                    let y = e.process([x, -x], [x, x]);
                    assert!(y[0].is_finite() && y[1].is_finite(), "{mode:?} {fs}: {y:?}");
                }
            }
        }
    }
}

#[test]
fn cpu_cost() {
    let mut bands = vec![];
    for i in 0..12 {
        let mut b = band(Shape::Bell, Slope::S24, 60.0 * 1.6f64.powi(i), if i % 2 == 0 { 4.0 } else { -4.0 }, 1.5);
        if i % 3 == 0 {
            b.dyn_range = -6.0;
        }
        bands.push(b);
    }
    for mode in [PMode::Zero, PMode::Natural, PMode::Linear] {
        let s = settings(mode, &bands);
        let mut e = engine(&s, 2);
        let n = FS as usize * 4;
        let t = std::time::Instant::now();
        let mut acc = 0.0f32;
        for i in 0..n {
            let x = ((i * 7919 % 1000) as f32 / 500.0 - 1.0) * 0.3;
            acc += e.process([x, x], [0.0, 0.0])[0];
        }
        eprintln!("{mode:?}: {:.1} % of real time, 12 bands (4 dynamic) {acc:.1}", t.elapsed().as_secs_f64() / 4.0 * 100.0);
    }
}

/// spectral dynamics: a loud resonance inside a wide band is pulled down, the quieter noise
/// around it (same band) is left alone, and the latency is still where the impulse is
#[test]
fn spectral_band_cuts_only_the_resonance() {
    let mut b = band(Shape::Bell, Slope::S12, 3000.0, 0.0, 0.5);
    b.dyn_range = -12.0;
    b.spectral = true;
    b.density = 0.0;
    b.thr_auto = false;
    b.thr = -40.0;
    let s = settings(PMode::Zero, &[b]);
    let mut e = engine(&s, 2);
    let lat = e.latency();
    assert!(lat > 0);
    let n = FS as usize * 2;
    let mut seed = 99u32;
    let (mut tone_in, mut tone_out, mut nz_in, mut nz_out) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    // measure with two separate passes: tone only, noise only, each through its own engine with
    // the other present (correlate by running the same mix and subtracting is overkill here:
    // look at the output spectrum at 3 kHz and at 2 kHz with Goertzel)
    let mut out = vec![0.0f32; n];
    let mut inp = vec![0.0f32; n];
    for i in 0..n {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let nz = ((seed >> 8) as f32 / 8388608.0 - 1.0) * 0.05;
        let x = (2.0 * PI * 3000.0 * i as f64 / FS).sin() as f32 * 0.5 + nz;
        inp[i] = x;
        out[i] = e.process([x, x], [0.0, 0.0])[0];
    }
    let goertzel = |v: &[f32], f: f64| {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (k, &x) in v.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * k as f64 / v.len() as f64).cos();
            let ph = 2.0 * PI * f * k as f64 / FS;
            re += w * x as f64 * ph.cos();
            im += w * x as f64 * ph.sin();
        }
        re * re + im * im
    };
    let band_pow = |v: &[f32], lo: f64, hi: f64| {
        let mut s = 0.0;
        let mut f = lo;
        while f < hi {
            s += goertzel(v, f);
            f += 37.0;
        }
        s
    };
    let a = n / 2;
    tone_in += goertzel(&inp[a - lat..n - lat], 3000.0);
    tone_out += goertzel(&out[a..], 3000.0);
    nz_in += band_pow(&inp[a - lat..n - lat], 1500.0, 2500.0) + band_pow(&inp[a - lat..n - lat], 3600.0, 5000.0);
    nz_out += band_pow(&out[a..], 1500.0, 2500.0) + band_pow(&out[a..], 3600.0, 5000.0);
    let tone_db = 10.0 * (tone_out / tone_in).log10();
    let nz_db = 10.0 * (nz_out / nz_in).log10();
    eprintln!("spectral: latency {lat}, tone {tone_db:.2} dB, noise around it {nz_db:.2} dB, shown {:.1}", e.spec.show.iter().cloned().fold(0.0f32, f32::min));
    assert!(tone_db < -8.0, "tone {tone_db}");
    assert!(nz_db > -1.5, "noise {nz_db}");
}
