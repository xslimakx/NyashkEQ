//! Every parameter id is unique and short enough for every host; the plugin builds its params.
use nih_plug::prelude::*;
use nyashkeq::NyashkEQ;
use std::collections::HashSet;

#[test]
fn parameter_ids_unique() {
    let p = NyashkEQ::default();
    let map = p.params().param_map();
    let mut seen = HashSet::new();
    for (id, _, _) in &map {
        assert!(seen.insert(id.clone()), "duplicate id {id}");
        assert!(id.len() <= 32, "id too long: {id}");
    }
    eprintln!("{} parameters", map.len());
    assert!(map.len() > 500);
}

#[test]
/// frequencies can be typed as notes, like in Pro-Q: "A4", "C#2+13", "Bb3", "C-1"
fn notes_parse_as_frequencies() {
    use nyashkeq::params::parse_hz;
    let near = |s: &str, f: f32| {
        let v = parse_hz(s).unwrap_or_else(|| panic!("{s} did not parse"));
        assert!((v / f - 1.0).abs() < 1e-3, "{s}: {v} vs {f}");
    };
    near("A4", 440.0);
    near("a4", 440.0);
    near("C4", 261.626);
    near("C#2", 69.296);
    near("C#2+13", 69.296 * 2f32.powf(13.0 / 1200.0));
    near("A4-20", 440.0 * 2f32.powf(-20.0 / 1200.0));
    near("Bb3", 233.082);
    near("B3", 246.942);
    near("C-1", 8.176);
    near("2k", 2000.0);
    near("2.5 kHz", 2500.0);
    near("100", 100.0);
    near("100 Hz", 100.0);
    assert!(parse_hz("H4").is_none());
}
