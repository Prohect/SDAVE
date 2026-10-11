//! Errors: the typed kind, the offset, the outer→inner path, and the observable surface.
//!
//! The expected `Display` strings are the formats documented on `Error`; the path ordering is the
//! documented outer→inner order.

use sdave_codec::{Error, ErrorKind, Limit, PATH_CAPACITY, Path, Step};

fn assert_is_error<E: core::error::Error>() {}

#[test]
fn an_error_is_a_core_error() {
    assert_is_error::<Error>();
}

#[test]
fn a_path_is_recorded_outer_to_inner() {
    let error = Error::new(ErrorKind::InvalidNumber)
        .at_index(2)
        .in_field("points");

    // The innermost step is recorded first, the outermost step ends up first on the path.
    assert_eq!(error.path().len(), 2);
    assert_eq!(
        error.path().steps().collect::<Vec<_>>(),
        vec![Step::Field("points"), Step::Index(2)]
    );
    assert_eq!(error.path().to_string(), "points[2]");
    assert!(!error.path().is_truncated());

    // A field after an index renders as a `.`-separated step.
    let nested = error.in_variant("Shape");
    assert_eq!(nested.path().to_string(), "Shape.points[2]");
}

#[test]
fn a_deep_path_keeps_its_outermost_steps() {
    let mut path = Path::EMPTY;
    for index in 0..(PATH_CAPACITY + 2) {
        path = path.pushed(Step::Index(index));
    }
    let steps = path.steps().collect::<Vec<_>>();
    assert_eq!(path.len(), PATH_CAPACITY);
    assert!(path.is_truncated());
    assert_eq!(steps[0], Step::Index(PATH_CAPACITY + 1));
    assert_eq!(steps[PATH_CAPACITY - 1], Step::Index(2));
    assert!(path.to_string().ends_with("..."));
}

#[test]
fn an_empty_path_is_empty() {
    assert!(Path::EMPTY.is_empty());
    assert_eq!(Path::EMPTY.to_string(), "");
    assert_eq!(Path::EMPTY.steps().count(), 0);
}

#[test]
fn an_offset_is_filled_in_once() {
    let error = Error::new(ErrorKind::InvalidNumber);
    assert_eq!(error.offset(), None);
    let located = error.with_offset(5);
    assert_eq!(located.offset(), Some(5));
    // A second fill-in must not overwrite the first.
    assert_eq!(located.with_offset(9).offset(), Some(5));
    assert_eq!(Error::at(ErrorKind::InvalidNumber, 12).offset(), Some(12));
}

#[test]
fn display_names_the_kind_the_offset_and_the_path() {
    let plain = Error::new(ErrorKind::MissingField { name: "arguments" });
    assert_eq!(plain.to_string(), "missing record field `arguments`");

    let located = Error::at(
        ErrorKind::WrongArity {
            expected: 2,
            actual: 3,
        },
        5,
    );
    assert_eq!(
        located.to_string(),
        "wrong item count: expected 2, got 3 [at offset 5]"
    );

    let pathed = Error::at(
        ErrorKind::WrongArity {
            expected: 2,
            actual: 3,
        },
        5,
    )
    .in_field("x");
    assert_eq!(
        pathed.to_string(),
        "wrong item count: expected 2, got 3 [at offset 5, path `x`]"
    );

    let limit = Error::new(ErrorKind::LimitExceeded {
        limit: Limit::Depth,
        value: 9,
        max: 8,
    });
    assert_eq!(limit.to_string(), "nesting depth exceeded: 9 > 8");

    let policy = Error::new(ErrorKind::InvalidNamePolicy);
    assert_eq!(
        policy.to_string(),
        "two whitelisted constructors abbreviate to the same short name"
    );

    let borrow = Error::new(ErrorKind::NotBorrowable);
    assert_eq!(
        borrow.to_string(),
        "the document unit cannot be borrowed as the requested text or bytes"
    );

    let unframable = Error::new(ErrorKind::Unframable);
    assert_eq!(
        unframable.to_string(),
        "no limiter pair of the profile can frame this payload"
    );

    let short = Error::new(ErrorKind::TargetExhausted {
        available: 4,
        reached: 12,
    });
    assert_eq!(
        short.to_string(),
        "the document target ran out of room: reached 12 of 4 units"
    );

    let allocation = Error::new(ErrorKind::Allocation);
    assert_eq!(
        allocation.to_string(),
        "the document target's allocator refused to grow"
    );
}

#[test]
fn equality_covers_the_kind_the_offset_and_the_path() {
    let subject = Error::at(ErrorKind::InvalidBoolean, 3).in_field("ok");
    assert_eq!(
        subject,
        Error::at(ErrorKind::InvalidBoolean, 3).in_field("ok")
    );
    assert_ne!(
        subject,
        Error::at(ErrorKind::InvalidBoolean, 4).in_field("ok")
    );
    assert_ne!(
        subject,
        Error::at(ErrorKind::InvalidBoolean, 3).in_field("no")
    );
    assert_ne!(
        subject,
        Error::at(ErrorKind::InvalidNumber, 3).in_field("ok")
    );
    assert_ne!(subject, Error::at(ErrorKind::InvalidBoolean, 3));
}
