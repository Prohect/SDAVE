//! Public grammar surface added in 0.2.3: `NonEnvelop` metadata trimming,
//! named-field header splitting, the stable profile fingerprint, and public
//! marker checking.

mod common;

use common::*;
use sdave::*;

#[test]
fn trims_ascii_formatting_and_classifies_gaps() {
    assert_eq!(trim_metadata(b""), b"");
    assert_eq!(trim_metadata(b" \t\r\n"), b"");
    assert_eq!(trim_metadata(b"x: u32"), b"x: u32");
    assert_eq!(trim_metadata(b"  x: u32\r\n"), b"x: u32");
    assert_eq!(trim_metadata(b" pair: (u32, u64) "), b"pair: (u32, u64)");
    assert_eq!(trim_metadata_range(b"  x: u32\r\n"), (2, 8));

    assert!(is_formatting(b' ') && is_formatting(b'\n'));
    assert!(!is_formatting(b'x') && !is_formatting(b':'));
}

#[test]
fn splits_field_headers_at_the_exact_separator() {
    let header =
        parse_field_header("inner: my_app::Vec<my_app::Map<u32, my_app::Entity<SharedString>>>")
            .unwrap();
    assert_eq!(header.name(), "inner");
    assert_eq!(
        header.ty(),
        "my_app::Vec<my_app::Map<u32, my_app::Entity<SharedString>>>"
    );

    let (name, ty) = parse_field_header("pair: (u32, u64)").unwrap().into_parts();
    assert_eq!((name, ty), ("pair", "(u32, u64)"));

    for malformed in ["", "field:Type", "x::y", ": Type", "name: ", "name:"] {
        assert!(
            matches!(
                parse_field_header(malformed).unwrap_err().kind,
                ErrorKind::InvalidFieldHeader(_)
            ),
            "expected {malformed:?} to be rejected"
        );
    }
}

fn pairs() -> Vec<LimiterPair<u8>> {
    vec![
        pair(b"~", b"^", 2),
        pair(b"?", b"!", 2),
        pair(b"{", b"}", 2),
        pair(b"#", b"@", 2),
    ]
}

fn config(variant: Variant) -> Config {
    Config::new(variant, pairs()).unwrap()
}

#[test]
fn fingerprint_is_stable_and_profile_sensitive() {
    let v1 = config(Variant::V1);
    assert_eq!(v1, config(Variant::V1));
    assert_eq!(v1.fingerprint(), config(Variant::V1).fingerprint());

    // Pair order is load-bearing: SDAVE selects the first matching pair.
    let reordered = Config::new(
        Variant::V1,
        vec![
            pair(b"?", b"!", 2),
            pair(b"~", b"^", 2),
            pair(b"{", b"}", 2),
            pair(b"#", b"@", 2),
        ],
    )
    .unwrap();
    assert_ne!(v1.fingerprint(), reordered.fingerprint());

    // The delimiter variant is part of the profile.
    assert_ne!(v1.fingerprint(), config(Variant::V2).fingerprint());

    // Repeat counts are part of the profile.
    let repeated = Config::new(
        Variant::V1,
        vec![
            pair(b"~", b"^", 3),
            pair(b"?", b"!", 2),
            pair(b"{", b"}", 2),
            pair(b"#", b"@", 2),
        ],
    )
    .unwrap();
    assert_ne!(v1.fingerprint(), repeated.fingerprint());

    // Limits are part of the profile.
    let limited = v1
        .clone()
        .with_limits(Limits {
            max_depth: 3,
            ..v1.limits().clone()
        })
        .unwrap();
    assert_ne!(v1.fingerprint(), limited.fingerprint());
}

#[test]
fn config_is_a_usable_map_key() {
    use std::collections::HashMap;
    let mut map = HashMap::new();
    map.insert(config(Variant::V1), 7u32);
    assert_eq!(map.get(&config(Variant::V1)), Some(&7));
}

#[test]
fn check_marker_is_public_and_policy_aware() {
    struct Names;
    impl NamePolicy for Names {
        const SHORT_TYPES: &'static [ShortType] =
            &[ShortType::of::<String>(), ShortType::of::<Vec<()>>()];
    }

    let decoder = Deserializer::<Names>::new(config(Variant::V1)).unwrap();
    assert!(decoder.check_marker::<String>("String", 0).is_ok());
    assert!(decoder.check_marker::<Vec<String>>("Vec<String>", 0).is_ok());
    // Tuple-comma padding stays equivalent through the public entry point.
    assert!(decoder.check_marker::<(u32, u64)>("(u32,u64)", 0).is_ok());

    let error = decoder.check_marker::<String>("Vec<String>", 7).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::TypeMismatch { .. }));
    assert_eq!(error.offset, Some(7));
}
