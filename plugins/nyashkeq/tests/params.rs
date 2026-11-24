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
