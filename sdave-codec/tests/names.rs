//! Markers: the `ShortType` / `NamePolicy` rules, and the marker comparison equivalence.
//!
//! Every expectation here is derived from the codec README's marker rules and from the documented
//! behaviour of `core::any::type_name`; none of it is read off observed output.

use sdave_codec::{
    ErrorKind, NamePolicy, ShortType, marker_eq_ascii, own_path, short_name, units_eq_ascii,
};

/// A constructor in this test crate, used to collide with `core::option::Option`.
#[allow(dead_code)]
struct Option;

/// A local type, to show that a non-whitelisted constructor keeps its full path.
#[allow(dead_code)]
struct Widget;

#[test]
fn an_exemplar_is_looked_up_by_its_own_constructor_path() {
    // `ShortType::of::<Vec<()>>()` selects `Vec`, covering every `Vec<T>`.
    let exemplar = ShortType::of::<Vec<()>>();
    assert!(exemplar.full_name().starts_with("alloc::vec::Vec<"));
    assert_eq!(exemplar.own_path(), "alloc::vec::Vec");
    assert_eq!(exemplar.short_name(), "Vec");
}

#[test]
fn a_whitelisted_constructor_emits_its_last_identifier() {
    static ENTRIES: &[ShortType] = &[ShortType::of::<Vec<()>>()];
    let policy = NamePolicy::new(ENTRIES);

    // Any instantiation of the registered constructor is covered, and the arguments stay
    // verbatim: the marker still names the instantiation, not just the constructor.
    assert_eq!(policy.render("alloc::vec::Vec<u8>"), "Vec<u8>");
    assert_eq!(policy.render("alloc::vec::Vec<()>"), "Vec<()>");

    // Everything else keeps its full path, verbatim.
    assert_eq!(
        policy.render("alloc::string::String"),
        "alloc::string::String"
    );
    assert_eq!(policy.render("u64"), "u64");

    // Matching uses the complete constructor path, never a suffix.
    assert_eq!(policy.render("names::vec::Vec<u8>"), "names::vec::Vec<u8>");
    assert_eq!(policy.render("other::Vec<u8>"), "other::Vec<u8>");

    // Shortening does not propagate across nesting: only the outer constructor's own path is
    // looked up, so the inner `String` keeps its full path, and shortening the outer constructor
    // to its last identifier keeps everything after that path verbatim.
    assert_eq!(
        policy.render("alloc::vec::Vec<alloc::string::String>"),
        "Vec<alloc::string::String>"
    );
}

#[test]
fn the_verbatim_policy_shortens_nothing() {
    let policy = NamePolicy::VERBATIM;
    assert!(policy.is_verbatim());
    assert!(policy.entries().is_empty());
    assert_eq!(policy.render("alloc::vec::Vec<u8>"), "alloc::vec::Vec<u8>");
    assert_eq!(policy.render("(u8, u16)"), "(u8, u16)");
    assert_eq!(policy.validate(), Ok(()));
}

#[test]
fn two_constructors_that_abbreviate_alike_invalidate_the_policy() {
    // `core::option::Option` and this crate's own `Option` both end in `Option`.
    static ENTRIES: &[ShortType] = &[
        ShortType::of::<core::option::Option<u8>>(),
        ShortType::of::<Option>(),
    ];
    assert!(ShortType::of::<Option>().own_path().ends_with("::Option"));

    let policy = NamePolicy::new(ENTRIES);
    let error = policy.validate().expect_err("collision must be rejected");
    assert_eq!(*error.kind(), ErrorKind::InvalidNamePolicy);

    // One entry twice is not a collision: it is the same constructor.
    static SAME: &[ShortType] = &[ShortType::of::<Vec<()>>(), ShortType::of::<Vec<()>>()];
    assert_eq!(NamePolicy::new(SAME).validate(), Ok(()));

    // Distinct short names are fine.
    static FINE: &[ShortType] = &[ShortType::of::<Vec<()>>(), ShortType::of::<Widget>()];
    assert_eq!(NamePolicy::new(FINE).validate(), Ok(()));
}

#[test]
fn rendering_a_local_type() {
    let exemplar = ShortType::of::<Widget>();
    assert!(exemplar.own_path().ends_with("::Widget"));
    assert_eq!(exemplar.short_name(), "Widget");
}

#[test]
fn path_extraction_leaves_non_path_spellings_alone() {
    // A tuple's own constructor is the whole spelling, even when an element is generic.
    assert_eq!(
        own_path("(alloc::vec::Vec<u8>, u8)"),
        "(alloc::vec::Vec<u8>, u8)"
    );
    assert_eq!(own_path("[u8; 4]"), "[u8; 4]");
    assert_eq!(own_path("&str"), "&str");
    // A qualified-path spelling is returned whole: its `<` does not open a generic argument list
    // of a path.
    assert_eq!(
        own_path("<T as core::ops::Deref>::Target"),
        "<T as core::ops::Deref>::Target"
    );
    // A reference to a generic is not a constructor path, so nothing is stripped.
    assert_eq!(own_path("&Vec<u8>"), "&Vec<u8>");

    assert_eq!(short_name("alloc::vec::Vec"), "Vec");
    assert_eq!(short_name("u8"), "u8");
}

#[test]
fn comma_padding_is_the_only_equivalence() {
    let verbatim = NamePolicy::VERBATIM;
    assert!(verbatim.accepts("(u8, u16)", "(u8, u16)"));
    assert!(verbatim.accepts("u8", "u8"));

    // A single `,` immediately before `)` is padding, in both directions.
    assert!(verbatim.accepts("(u8,)", "(u8)"));
    assert!(verbatim.accepts("(u8)", "(u8,)"));
    assert!(!verbatim.accepts("(u8, u16)", "(u16, u8)"));
    assert!(!verbatim.accepts("u8", "u16"));
    // Nothing else is normalised: a space is not comma padding.
    assert!(!verbatim.accepts("(u8, u16)", "(u8,u16)"));
}

#[test]
fn comparison_is_symmetric_about_the_policy() {
    static ENTRIES: &[ShortType] = &[ShortType::of::<Vec<()>>()];
    let policy = NamePolicy::new(ENTRIES);

    // The sender rendered what this policy renders: the constructor's path shortened, its
    // arguments verbatim.
    assert!(policy.accepts("alloc::vec::Vec<u8>", "Vec<u8>"));
    // The sender kept the verbatim spelling of the expected type.
    assert!(policy.accepts("alloc::vec::Vec<u8>", "alloc::vec::Vec<u8>"));
    // A different type is still a mismatch.
    assert!(!policy.accepts("alloc::vec::Vec<u8>", "String"));
}

#[test]
fn markers_are_compared_unit_by_unit_against_ascii() {
    assert!(units_eq_ascii(&['V', 'e', 'c'], "Vec"));
    assert!(units_eq_ascii::<u8>(b"Vec", "Vec"));
    assert!(!units_eq_ascii(&['V', 'e', 'c'], "Vec "));
    assert!(!units_eq_ascii(&['V', 'e'], "Vec"));
    // A non-ASCII unit can never stand for ASCII marker text.
    assert!(!units_eq_ascii(&['\u{2666}'], "\u{2666}"));

    assert!(marker_eq_ascii(&['(', 'u', '8', ',', ')'], "(u8)"));
    assert!(marker_eq_ascii(&['(', 'u', '8', ')'], "(u8,)"));
    assert!(!marker_eq_ascii(
        &['(', 'u', '8', ',', ' ', 'u', '1', '6', ')'],
        "(u8,u16)"
    ));

    static ENTRIES: &[ShortType] = &[ShortType::of::<Vec<()>>()];
    let policy = NamePolicy::new(ENTRIES);
    assert!(policy.accepts_units::<u8>("alloc::vec::Vec<u8>", b"Vec<u8>"));
    assert!(
        policy.accepts_units::<char>("alloc::vec::Vec<u8>", &['V', 'e', 'c', '<', 'u', '8', '>'])
    );
    assert!(!policy.accepts_units::<u8>("alloc::vec::Vec<u8>", b"Alloc"));
}
