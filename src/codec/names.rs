//! Compiler-derived type markers and immutable constructor abbreviations.
//!
//! Rendering preserves the compiler's spelling apart from whitelisted paths.
//! Marker comparison ignores only ASCII formatting next to tuple-member commas;
//! metadata trimming and named-field splitting belong to the enclosing codec.
//! Raw source identifiers use the compiler's unraw spelling, not an `r#` alias.
//! Compiler-reported lifetime spellings are preserved when present.

use std::collections::BTreeMap;

use crate::error_new;

use super::error::{Error, ErrorKind, Result};
use super::grammar::is_formatting;

/// A type exemplar selecting its root named constructor for abbreviation.
///
/// Generic arguments do not constrain the selection and are not themselves
/// selected. For example, `ShortType::of::<Vec<String>>()` selects `Vec`, not
/// `String`, and applies to every instantiation of `Vec`.
#[derive(Clone, Copy, Debug)]
pub struct ShortType {
    type_name: fn() -> &'static str,
}

impl ShortType {
    /// Store a monomorphized compiler-name function without evaluating it in
    /// const context. Aliases are resolved by the compiler when it is called.
    pub const fn of<T: ?Sized>() -> Self {
        Self {
            type_name: std::any::type_name::<T>,
        }
    }
}

/// An immutable, compile-time list of constructors to abbreviate.
///
/// Entries cannot supply replacement names: the last identifier of each
/// compiler-reported constructor path is its only possible abbreviation.
pub trait NamePolicy {
    const SHORT_TYPES: &'static [ShortType];
}

/// Abbreviations for common standard constructors. Primitives are already short.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultNames;

impl NamePolicy for DefaultNames {
    const SHORT_TYPES: &'static [ShortType] = &[
        ShortType::of::<String>(),
        ShortType::of::<Vec<()>>(),
        ShortType::of::<Option<()>>(),
        ShortType::of::<std::result::Result<(), ()>>(),
        ShortType::of::<Box<()>>(),
        // This may name a generic NonZero constructor rather than the alias.
        ShortType::of::<std::num::NonZeroU32>(),
    ];
}

/// SDAVE's fixed naming policy for a value erased behind a trait object.
///
/// A `dyn Trait` vtable cannot be generic over a caller's [`NamePolicy`], so an erased
/// value's own document is rendered with this canonical policy, sharing the caller's
/// framing [`Config`](crate::Config). The `dyn` **marker** itself is rendered by the
/// caller's policy (see [`Parser`]'s handling of `dyn`); this only fixes the inner value.
#[derive(Clone, Copy, Debug, Default)]
pub struct DynNames;

impl NamePolicy for DynNames {
    const SHORT_TYPES: &'static [ShortType] = <DefaultNames as NamePolicy>::SHORT_TYPES;
}

/// Render a nonempty type marker using one validated naming policy.
///
/// Supported expressions are named paths with type/lifetime arguments and simple
/// decimal integer, bool, or character const arguments, tuples, references, slices,
/// arrays with decimal unsigned lengths, and trait objects (`dyn path::Trait`, without
/// `+` bounds). Compilers may omit lifetimes; when reported in generic arguments or
/// references, their spelling is preserved.
/// Integer literals are unsuffixed decimal; const literal spellings are not
/// value-normalized.
/// Nesting is bounded to 128 levels. Function types and complex const arguments return
/// `InvalidTypeExpression` rather than being guessed at.
pub fn type_marker<T: ?Sized, P: NamePolicy>() -> Result<String> {
    Names::new::<P>()?.render::<T>()
}

/// A validated, owned policy snapshot. There is no mutable global name table.
#[derive(Clone, Debug)]
pub(super) struct Names {
    short_by_path: BTreeMap<&'static str, &'static str>,
}

impl Names {
    pub(super) fn new<P: NamePolicy>() -> Result<Self> {
        let mut short_by_path: BTreeMap<&'static str, &'static str> = BTreeMap::new();
        let mut path_by_short: BTreeMap<&'static str, &'static str> = BTreeMap::new();

        for entry in P::SHORT_TYPES {
            let source = (entry.type_name)();
            let expression = Expression::parse(source).map_err(|error| {
                error_new!(ErrorKind::InvalidNamePolicy(format!(
                    "cannot select a constructor from {source:?}: {error}"
                )))
            })?;
            let path = expression.root_constructor.ok_or_else(|| {
                error_new!(ErrorKind::InvalidNamePolicy(format!(
                    "shortlist exemplar {source:?} must have a named root constructor"
                )))
            })?;
            let short = path.rsplit("::").next().unwrap_or(path);

            // Different instantiations and aliases select the same constructor.
            if short_by_path.contains_key(path) {
                continue;
            }
            if let Some(other) = path_by_short.get(short) {
                return Err(error_new!(ErrorKind::InvalidNamePolicy(format!(
                    "constructors {other:?} and {path:?} both abbreviate to {short:?}"
                ))));
            }
            short_by_path.insert(path, short);
            path_by_short.insert(short, path);
        }

        Ok(Self { short_by_path })
    }

    pub(super) fn render<T: ?Sized>(&self) -> Result<String> {
        Ok(Expression::parse(std::any::type_name::<T>())?.render(self))
    }

    /// Compare an incoming marker with an already policy-rendered expectation.
    /// Neither side is abbreviated here: a qualified spelling cannot bypass a
    /// policy requiring a short spelling. No outer metadata trimming is done.
    pub(super) fn equivalent(&self, actual: &str, expected: &str) -> Result<bool> {
        let actual = Expression::parse(actual)?;
        let expected = Expression::parse(expected)?;
        Ok(actual.without_tuple_padding() == expected.without_tuple_padding())
    }
}

// Bound every recursive type edge, including references and generic children.
const MAX_TYPE_DEPTH: usize = 128;

#[derive(Debug)]
struct Constructor<'a> {
    path: &'a str,
    start: usize,
    end: usize,
}

/// Spans preserve every byte not explicitly changed by the naming policy.
/// Only syntactic tuple commas are recorded, never generic or literal commas.
#[derive(Debug)]
struct Expression<'a> {
    text: &'a str,
    root_constructor: Option<&'a str>,
    constructors: Vec<Constructor<'a>>,
    tuple_commas: Vec<usize>,
}

impl<'a> Expression<'a> {
    fn parse(text: &'a str) -> Result<Self> {
        let mut parser = Parser {
            text,
            pos: 0,
            constructors: Vec::new(),
            tuple_commas: Vec::new(),
        };
        let root_constructor = parser.parse_type(0)?;
        parser.skip_formatting();
        if parser.pos != text.len() {
            return Err(parser.error("unexpected trailing syntax"));
        }
        Ok(Self {
            text,
            root_constructor,
            constructors: parser.constructors,
            tuple_commas: parser.tuple_commas,
        })
    }

    fn render(&self, names: &Names) -> String {
        let mut rendered = String::with_capacity(self.text.len());
        let mut copied = 0;
        for constructor in &self.constructors {
            rendered.push_str(&self.text[copied..constructor.start]);
            if let Some(short) = names.short_by_path.get(constructor.path) {
                rendered.push_str(short);
            } else {
                rendered.push_str(constructor.path);
            }
            copied = constructor.end;
        }
        rendered.push_str(&self.text[copied..]);
        rendered
    }

    fn without_tuple_padding(&self) -> String {
        let bytes = self.text.as_bytes();
        let mut normalized = String::with_capacity(bytes.len());
        let mut copied = 0;
        // The parser visits commas in source order. Padding never crosses a
        // punctuation token, including a comma belonging to another level.
        for &comma in &self.tuple_commas {
            let mut before = comma;
            while before > copied && is_formatting(bytes[before - 1]) {
                before -= 1;
            }
            let mut after = comma + 1;
            while after < bytes.len() && is_formatting(bytes[after]) {
                after += 1;
            }
            normalized.push_str(&self.text[copied..before]);
            normalized.push(',');
            copied = after;
        }
        normalized.push_str(&self.text[copied..]);
        normalized
    }
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    constructors: Vec<Constructor<'a>>,
    tuple_commas: Vec<usize>,
}

impl<'a> Parser<'a> {
    fn parse_type(&mut self, depth: usize) -> Result<Option<&'a str>> {
        self.check_depth(depth)?;
        self.skip_formatting();
        match self.peek() {
            Some(b'(') => self.parse_tuple(depth).map(|()| None),
            Some(b'[') => self.parse_sequence(depth).map(|()| None),
            Some(b'&') => self.parse_reference(depth).map(|()| None),
            _ if self.at_dyn_keyword() => self.parse_dyn(depth).map(|()| None),
            _ => self.parse_constructor(depth).map(Some),
        }
    }

    fn parse_constructor(&mut self, depth: usize) -> Result<&'a str> {
        let start = self.pos;
        self.parse_identifier()?;
        while self.starts_with("::") {
            self.pos += 2;
            self.parse_identifier()?;
        }
        let end = self.pos;
        let path = &self.text[start..end];
        self.constructors.push(Constructor { path, start, end });

        self.skip_formatting();
        if self.eat(b'<') {
            self.skip_formatting();
            if self.peek() == Some(b'>') {
                return Err(self.error("empty generic argument list"));
            }
            loop {
                self.parse_argument(depth + 1)?;
                self.skip_formatting();
                if self.eat(b'>') {
                    break;
                }
                self.expect(b',', "expected ',' or '>' after generic argument")?;
                self.skip_formatting();
                if self.peek() == Some(b'>') {
                    return Err(self.error("missing generic argument after comma"));
                }
            }
        }
        Ok(path)
    }

    fn parse_dyn(&mut self, depth: usize) -> Result<()> {
        // Rust renders a trait object as `dyn path::Trait`, including the space. The
        // `dyn ` prefix is kept verbatim and the trait path is a constructor, so it is
        // abbreviated by the same policy as any other constructor. A vtable cannot be
        // generic over the policy, so SDAVE owns this canonical `dyn` spelling.
        self.pos += 3; // "dyn"
        self.skip_formatting();
        self.parse_type(depth + 1).map(|_| ())
    }

    /// does `self.pos` start the `dyn` keyword (rather than an identifier such as `dyn_x`)?
    fn at_dyn_keyword(&self) -> bool {
        self.starts_with("dyn")
            && self
                .text
                .get(self.pos + 3..)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|ch| !is_identifier_continue(ch))
    }

    fn parse_argument(&mut self, depth: usize) -> Result<()> {
        self.check_depth(depth)?;
        self.skip_formatting();
        match self.peek() {
            Some(b'\'') => self.parse_quote_argument(),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_integer(true),
            _ if self.parse_boolean() => Ok(()),
            _ => self.parse_type(depth).map(|_| ()),
        }
    }

    fn parse_tuple(&mut self, depth: usize) -> Result<()> {
        self.pos += 1; // '('
        self.skip_formatting();
        if self.eat(b')') {
            return Ok(());
        }
        self.parse_type(depth + 1)?;
        self.skip_formatting();
        self.expect(
            b',',
            "expected tuple comma; parenthesized types are unsupported",
        )?;
        self.tuple_commas.push(self.pos - 1);
        loop {
            self.skip_formatting();
            if self.eat(b')') {
                return Ok(());
            }
            self.parse_type(depth + 1)?;
            self.skip_formatting();
            if self.eat(b')') {
                return Ok(());
            }
            self.expect(b',', "expected ',' or ')' after tuple member")?;
            self.tuple_commas.push(self.pos - 1);
        }
    }

    fn parse_reference(&mut self, depth: usize) -> Result<()> {
        self.pos += 1; // '&'
        self.skip_formatting();
        if self.peek() == Some(b'\'') {
            self.parse_lifetime()?;
            self.skip_formatting();
        }
        if self.starts_with("mut")
            && self
                .text
                .as_bytes()
                .get(self.pos + 3)
                .copied()
                .is_some_and(is_formatting)
        {
            self.pos += 3;
            self.skip_formatting();
        }
        self.parse_type(depth + 1).map(|_| ())
    }

    fn parse_sequence(&mut self, depth: usize) -> Result<()> {
        self.pos += 1; // '['
        self.parse_type(depth + 1)?;
        self.skip_formatting();
        if self.eat(b';') {
            self.skip_formatting();
            self.parse_integer(false)?;
            self.skip_formatting();
        }
        self.expect(b']', "expected ']' after slice or array")
    }

    fn parse_integer(&mut self, signed: bool) -> Result<()> {
        if signed {
            self.eat(b'-');
        }
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.error("expected a decimal integer const literal"));
        }
        // No numeric conversion: even hostile long literals cannot overflow.
        Ok(())
    }

    fn parse_boolean(&mut self) -> bool {
        for literal in ["true", "false"] {
            if self.starts_with(literal) {
                let end = self.pos + literal.len();
                let boundary = match self.text.as_bytes().get(end).copied() {
                    None | Some(b',' | b'>') => true,
                    Some(byte) => is_formatting(byte),
                };
                if boundary {
                    self.pos = end;
                    return true;
                }
            }
        }
        false
    }

    fn parse_quote_argument(&mut self) -> Result<()> {
        // A character has one scalar followed by a closing quote, or starts
        // with an escape. A lifetime is an apostrophe plus an unquoted ident.
        let character = self
            .text
            .get(self.pos..)
            .and_then(|rest| rest.strip_prefix('\''))
            .is_some_and(|rest| {
                let mut chars = rest.chars();
                matches!(chars.next(), Some('\\')) || chars.next() == Some('\'')
            });
        if character {
            self.parse_character()
        } else {
            self.parse_lifetime()
        }
    }

    fn parse_lifetime(&mut self) -> Result<()> {
        self.expect(b'\'', "expected a lifetime")?;
        self.parse_identifier()?;
        if self.peek() == Some(b'\'') {
            return Err(self.error("a lifetime must not have a closing quote"));
        }
        Ok(())
    }

    fn parse_character(&mut self) -> Result<()> {
        self.expect(b'\'', "expected a character const literal")?;
        if self.eat(b'\\') {
            self.parse_escape()?;
        } else {
            let Some(ch) = self.peek_char() else {
                return Err(self.error("unfinished character const literal"));
            };
            if ch == '\'' || ch.is_control() {
                return Err(self.error("expected one character or a character escape"));
            }
            self.pos += ch.len_utf8();
        }
        self.expect(b'\'', "expected closing quote after one character")
    }

    fn parse_escape(&mut self) -> Result<()> {
        match self.peek() {
            Some(b'\'' | b'"' | b'\\' | b'n' | b'r' | b't' | b'0') => {
                self.pos += 1;
                Ok(())
            }
            Some(b'x') => {
                self.pos += 1;
                let high = self.parse_hex_digit()?;
                let low = self.parse_hex_digit()?;
                if high * 16 + low > 0x7f {
                    return Err(self.error("character hex escape must be ASCII"));
                }
                Ok(())
            }
            Some(b'u') => {
                self.pos += 1;
                self.expect(b'{', "expected '{' in Unicode character escape")?;
                let mut value = 0;
                let mut digits = 0;
                while let Some(digit) = self.peek().and_then(hex_value) {
                    if digits == 6 {
                        return Err(self.error("Unicode character escape is too long"));
                    }
                    self.pos += 1;
                    value = value * 16 + digit;
                    digits += 1;
                }
                if digits == 0 {
                    return Err(self.error("empty Unicode character escape"));
                }
                self.expect(b'}', "expected '}' in Unicode character escape")?;
                if char::from_u32(value).is_none() {
                    return Err(self.error("Unicode escape is not a scalar value"));
                }
                Ok(())
            }
            _ => Err(self.error("unsupported character escape")),
        }
    }

    fn parse_hex_digit(&mut self) -> Result<u32> {
        let Some(value) = self.peek().and_then(hex_value) else {
            return Err(self.error("expected a hexadecimal escape digit"));
        };
        self.pos += 1;
        Ok(value)
    }

    fn parse_identifier(&mut self) -> Result<()> {
        let Some(first) = self.peek_char() else {
            return Err(self.error("expected a type or identifier"));
        };
        if !is_identifier_start(first) {
            return Err(self.error("expected an unraw Rust identifier"));
        }
        self.pos += first.len_utf8();
        while let Some(ch) = self.peek_char() {
            if !is_identifier_continue(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn peek_char(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    fn starts_with(&self, text: &str) -> bool {
        self.text
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(text))
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8, message: &str) -> Result<()> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    fn skip_formatting(&mut self) {
        while self.peek().is_some_and(is_formatting) {
            self.pos += 1;
        }
    }

    fn check_depth(&self, depth: usize) -> Result<()> {
        if depth >= MAX_TYPE_DEPTH {
            Err(self.error("type-expression nesting exceeds 128 levels"))
        } else {
            Ok(())
        }
    }

    fn error(&self, message: &str) -> Error {
        // The codec supplies absolute input offsets; this is marker-local.
        error_new!(ErrorKind::InvalidTypeExpression(format!(
            "{message} at type-expression byte {}",
            self.pos
        )))
    }
}


fn is_identifier_start(ch: char) -> bool {
    // Preserve non-ASCII identifiers opaquely, including combining marks. The
    // compiler is the naming authority, not a second Unicode/XID implementation.
    ch == '_'
        || ch.is_ascii_alphabetic()
        || (!ch.is_ascii() && !ch.is_whitespace() && !ch.is_control())
}

fn is_identifier_continue(ch: char) -> bool {
    is_identifier_start(ch) || ch.is_ascii_digit()
}

fn hex_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'a'..=b'f' => Some(u32::from(byte - b'a') + 10),
        b'A'..=b'F' => Some(u32::from(byte - b'A') + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::type_name;
    use std::marker::PhantomData;

    struct Unknown;
    struct Wrapper<T> {
        _value: PhantomData<T>,
    }
    struct Constants<const N: i32, const B: bool, const C: char>;
    struct Borrowed<'a> {
        _value: &'a str,
    }
    #[allow(non_camel_case_types)]
    struct r#type;

    mod left {
        pub struct Thing<T> {
            _value: std::marker::PhantomData<T>,
        }
    }
    mod right {
        pub struct Thing<T> {
            _value: std::marker::PhantomData<T>,
        }
    }

    struct NoNames;
    impl NamePolicy for NoNames {
        const SHORT_TYPES: &'static [ShortType] = &[];
    }
    struct VecNames;
    impl NamePolicy for VecNames {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<Vec<()>>()];
    }
    struct VecExemplar;
    impl NamePolicy for VecExemplar {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<Vec<String>>()];
    }
    struct StringNames;
    impl NamePolicy for StringNames {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<String>()];
    }
    struct LeftNames;
    impl NamePolicy for LeftNames {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<left::Thing<()>>()];
    }

    type StringAlias = String;
    type LeftAlias = left::Thing<u32>;
    struct Aliases;
    impl NamePolicy for Aliases {
        const SHORT_TYPES: &'static [ShortType] = &[
            ShortType::of::<String>(),
            ShortType::of::<StringAlias>(),
            ShortType::of::<left::Thing<()>>(),
            ShortType::of::<LeftAlias>(),
            ShortType::of::<left::Thing<String>>(),
        ];
    }
    struct Ambiguous;
    impl NamePolicy for Ambiguous {
        const SHORT_TYPES: &'static [ShortType] = &[
            ShortType::of::<left::Thing<()>>(),
            ShortType::of::<right::Thing<()>>(),
        ];
    }
    struct ReverseAmbiguous;
    impl NamePolicy for ReverseAmbiguous {
        const SHORT_TYPES: &'static [ShortType] = &[
            ShortType::of::<right::Thing<()>>(),
            ShortType::of::<left::Thing<()>>(),
        ];
    }
    struct StructuralEntry;
    impl NamePolicy for StructuralEntry {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<()>()];
    }
    struct RawNames;
    impl NamePolicy for RawNames {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<r#type>()];
    }

    fn constructor<T: ?Sized>() -> &'static str {
        Expression::parse(type_name::<T>())
            .unwrap()
            .root_constructor
            .unwrap()
    }

    #[test]
    fn default_standard_constructors_and_primitives() {
        assert_eq!(type_marker::<String, DefaultNames>().unwrap(), "String");
        assert_eq!(
            type_marker::<Vec<String>, DefaultNames>().unwrap(),
            "Vec<String>"
        );
        assert_eq!(
            type_marker::<Option<String>, DefaultNames>().unwrap(),
            "Option<String>"
        );
        assert_eq!(
            type_marker::<Box<String>, DefaultNames>().unwrap(),
            "Box<String>"
        );
        assert_eq!(
            type_marker::<std::result::Result<String, bool>, DefaultNames>().unwrap(),
            "Result<String, bool>"
        );
        assert_eq!(type_marker::<u32, DefaultNames>().unwrap(), "u32");
        assert_eq!(type_marker::<str, DefaultNames>().unwrap(), "str");

        // Do not freeze whether this toolchain reports an alias or a generic.
        let full = type_name::<std::num::NonZeroU32>();
        let path = constructor::<std::num::NonZeroU32>();
        let short = path.rsplit("::").next().unwrap();
        assert_eq!(
            type_marker::<std::num::NonZeroU32, DefaultNames>().unwrap(),
            format!("{short}{}", &full[path.len()..])
        );
    }

    #[test]
    fn parents_and_unknown_children_are_independent() {
        let unknown = type_name::<Unknown>();
        assert_eq!(
            type_marker::<Vec<Unknown>, DefaultNames>().unwrap(),
            format!("Vec<{unknown}>")
        );
        assert_eq!(
            type_marker::<Vec<Vec<Unknown>>, DefaultNames>().unwrap(),
            format!("Vec<Vec<{unknown}>>")
        );
        let wrapper = constructor::<Wrapper<()>>();
        assert_eq!(
            type_marker::<Wrapper<Vec<String>>, DefaultNames>().unwrap(),
            format!("{wrapper}<Vec<String>>")
        );
    }

    #[test]
    fn match_complete_paths_not_suffixes() {
        let right = constructor::<right::Thing<()>>();
        assert_eq!(
            type_marker::<right::Thing<left::Thing<u8>>, LeftNames>().unwrap(),
            format!("{right}<Thing<u8>>")
        );
        assert_eq!(
            type_marker::<left::Thing<right::Thing<u8>>, LeftNames>().unwrap(),
            format!("Thing<{right}<u8>>")
        );
    }

    #[test]
    fn exemplar_arguments_do_not_select_children_or_limit_instantiations() {
        let names = Names::new::<VecExemplar>().unwrap();
        assert_eq!(names.render::<Vec<u16>>().unwrap(), "Vec<u16>");
        assert_eq!(
            names.render::<Vec<String>>().unwrap(),
            format!("Vec<{}>", type_name::<String>())
        );
    }

    #[test]
    fn alternative_const_policies_are_independent_snapshots() {
        let vectors = Names::new::<VecNames>().unwrap();
        let strings = Names::new::<StringNames>().unwrap();
        let full = Names::new::<NoNames>().unwrap();
        let defaults = Names::new::<DefaultNames>().unwrap();
        assert_eq!(
            strings.render::<Vec<String>>().unwrap(),
            format!("{}<String>", constructor::<Vec<()>>())
        );
        assert_eq!(defaults.render::<Vec<String>>().unwrap(), "Vec<String>");
        assert_eq!(
            full.render::<Vec<String>>().unwrap(),
            type_name::<Vec<String>>()
        );
        assert_eq!(
            vectors.render::<Vec<String>>().unwrap(),
            format!("Vec<{}>", type_name::<String>())
        );
        assert_eq!(
            vectors.clone().render::<Vec<String>>().unwrap(),
            vectors.render::<Vec<String>>().unwrap()
        );
    }

    #[test]
    fn aliases_and_repeated_instantiations_are_deduplicated() {
        let names = Names::new::<Aliases>().unwrap();
        assert_eq!(names.short_by_path.len(), 2);
        assert_eq!(
            names.render::<left::Thing<String>>().unwrap(),
            "Thing<String>"
        );
    }

    #[test]
    fn ambiguous_short_identifiers_are_rejected_in_either_order() {
        for error in [
            Names::new::<Ambiguous>().unwrap_err(),
            Names::new::<ReverseAmbiguous>().unwrap_err(),
        ] {
            assert!(matches!(&error.kind, ErrorKind::InvalidNamePolicy(_)));
        }
    }

    #[test]
    fn shortlist_entries_need_named_root_constructors() {
        let error = Names::new::<StructuralEntry>().unwrap_err();
        assert!(matches!(&error.kind, ErrorKind::InvalidNamePolicy(_)));
    }

    #[test]
    fn tuple_comma_padding_is_equivalent_at_each_nesting_level() {
        let names = Names::new::<DefaultNames>().unwrap();
        for actual in ["Option<(u32,u64)>", "Option<(u32 \t,\r\nu64)>"] {
            assert!(names.equivalent(actual, "Option<(u32, u64)>").unwrap());
        }
        assert!(
            names
                .equivalent(
                    "(u32 \r\n,\tOption<(u8\t,\r u16)> , (bool \n, \t))",
                    "(u32, Option<(u8, u16)>, (bool,))",
                )
                .unwrap()
        );
        assert!(names.equivalent("(u32 \n,\t)", "(u32,)").unwrap());
        assert!(names.equivalent("()", "()").unwrap());
        assert!(!names.equivalent("( )", "()").unwrap());
        assert!(
            !names
                .equivalent("((u32,u64),u8)", "(u32,(u64,u8))")
                .unwrap()
        );
        assert!(!names.equivalent("(u32,u64,)", "(u32,u64)").unwrap());
        assert!(
            names
                .equivalent("&[(u8,\tu16); 3]", "&[(u8, u16); 3]")
                .unwrap()
        );
    }

    #[test]
    fn generic_comma_padding_remains_exact_even_around_tuples() {
        let names = Names::new::<DefaultNames>().unwrap();
        for actual in [
            "Result<u32,u64>",
            "Result<u32,  u64>",
            "Result<u32 , u64>",
            "Result<u32,\tu64>",
        ] {
            assert!(!names.equivalent(actual, "Result<u32, u64>").unwrap());
        }
        let expected = "Result<(u32, u64), (u8, u16)>";
        assert!(
            names
                .equivalent("Result<(u32,u64), (u8,\tu16)>", expected)
                .unwrap()
        );
        assert!(
            !names
                .equivalent("Result<(u32,u64),(u8,u16)>", expected)
                .unwrap()
        );
        assert!(
            names
                .equivalent(
                    "(Result<u32, u64>\t,\r(u8,u16))",
                    "(Result<u32, u64>, (u8, u16))",
                )
                .unwrap()
        );
    }

    #[test]
    fn other_spelling_and_metadata_are_not_normalized() {
        let names = Names::new::<DefaultNames>().unwrap();
        assert!(!names.equivalent(type_name::<String>(), "String").unwrap());
        let full = Names::new::<NoNames>().unwrap();
        let expected = full.render::<Vec<String>>().unwrap();
        assert!(!full.equivalent("Vec<String>", &expected).unwrap());
        assert!(full.equivalent(&expected, &expected).unwrap());
        for (actual, expected) in [
            (" String", "String"),
            ("String \r\n", "String"),
            ("Option< (u32,u64)>", "Option<(u32, u64)>"),
            ("( u32,u64)", "(u32, u64)"),
            ("(u32,u64 )", "(u32, u64)"),
            ("&mut  u8", "&mut u8"),
            ("[u8;3]", "[u8; 3]"),
        ] {
            assert!(!names.equivalent(actual, expected).unwrap());
        }
        for actual in [
            "field: String",
            "field:String",
            "alloc: :string::String",
            "alloc ::string::String",
        ] {
            assert!(names.equivalent(actual, "String").is_err());
        }
        assert!(names.equivalent("(u8,\u{00a0}u16)", "(u8, u16)").is_err());
        assert!(names.equivalent("u8", "").is_err());
    }

    #[test]
    fn structural_forms_and_const_literals_preserve_compiler_spelling() {
        let names = Names::new::<NoNames>().unwrap();
        type Structural = ((), (u8,), &'static [u16], &'static mut [u32; 3]);
        assert_eq!(
            names.render::<Structural>().unwrap(),
            type_name::<Structural>()
        );
        assert_eq!(names.render::<[u8]>().unwrap(), "[u8]");
        assert_eq!(
            names.render::<Constants<-7, true, ','>>().unwrap(),
            type_name::<Constants<-7, true, ','>>()
        );
        assert_eq!(
            names.render::<Constants<1, false, '\n'>>().unwrap(),
            type_name::<Constants<1, false, '\n'>>()
        );
        assert_eq!(
            names.render::<Constants<1, true, '\''>>().unwrap(),
            type_name::<Constants<1, true, '\''>>()
        );
        assert_eq!(
            type_marker::<&mut Vec<String>, DefaultNames>().unwrap(),
            "&mut Vec<String>"
        );
        assert_eq!(
            type_marker::<[String; 2], DefaultNames>().unwrap(),
            "[String; 2]"
        );

        for expression in [
            "G<-7, true, ','>",
            "G<0, false, 'λ'>",
            r"G<'\u{1f600}', '\x7f', '\'', '\\'>",
            "app::Δ<app::e\u{301}, (u8, u16)>",
        ] {
            let parsed = Expression::parse(expression).unwrap();
            assert_eq!(parsed.render(&names), expression);
        }
        let long_integer = format!("G<{}>", "9".repeat(512));
        assert!(Expression::parse(&long_integer).is_ok());
    }

    #[test]
    fn literal_commas_and_alternate_literal_spellings_are_not_tuple_padding() {
        let names = Names::new::<DefaultNames>().unwrap();
        assert!(names.equivalent("(G<','> ,\tu8)", "(G<','>, u8)").unwrap());
        assert!(!names.equivalent("G<',',true>", "G<',', true>").unwrap());
        assert!(!names.equivalent(r"G<'\x61'>", "G<'a'>").unwrap());
        assert!(!names.equivalent("G<' '>", "G<','>").unwrap());
    }

    #[test]
    fn raw_source_identifiers_use_the_compilers_unraw_spelling() {
        assert!(type_name::<r#type>().ends_with("::type"));
        assert_eq!(
            type_marker::<r#type, NoNames>().unwrap(),
            type_name::<r#type>()
        );
        assert_eq!(type_marker::<r#type, RawNames>().unwrap(), "type");
        assert!(
            Names::new::<RawNames>()
                .unwrap()
                .equivalent("r#type", "type")
                .is_err()
        );
    }

    #[test]
    fn malformed_and_unsupported_expressions_are_typed_errors() {
        for expression in [
            "",
            " \t\r\n",
            "::Thing",
            "foo::",
            "Vec<>",
            "Vec<,u32>",
            "Vec<u32,,u64>",
            "Vec<u32,>",
            "Vec<u32",
            "Vec<u32>>",
            "(u32)",
            "(,)",
            "(u32,,u64)",
            "(u32,u64",
            "[u8",
            "[; 3]",
            "[u8; -1]",
            "[u8; nope]",
            "[u8; 1 + 2]",
            "&",
            "&'static",
            "fn(u8) -> u8",
            "extern \"C\" fn()",
            "field: u8",
            "G<{ 1 + 2 }>",
            "G<1.0>",
            "G<0xff>",
            "G<1u8>",
            "G<true + false>",
            "G<'a",
            "G<'ab'>",
            r"G<'\q'>",
            r"G<'\x0'>",
            r"G<'\x80'>",
            r"G<'\u{}'>",
            r"G<'\u{d800}'>",
            r"G<'\u{110000}'>",
            r"G<'\u{1234567}'>",
        ] {
            let error = Expression::parse(expression).unwrap_err();
            assert!(
                matches!(&error.kind, ErrorKind::InvalidTypeExpression(_)),
                "{expression:?}: {error}"
            );
        }
        let error = type_marker::<fn(), DefaultNames>().unwrap_err();
        assert!(matches!(&error.kind, ErrorKind::InvalidTypeExpression(_)));
    }

    #[test]
    fn trait_object_dyn_spellings_are_accepted_and_render_canonically() {
        trait Obj {}
        trait Other {}

        let names = Names::new::<DefaultNames>().unwrap();
        let dyn_obj = std::any::type_name::<dyn Obj>();
        // The `dyn` spelling is preserved (a test-local trait is not whitelisted), and
        // Box/Vec still abbreviate. One marker per trait, distinct traits differ.
        assert_eq!(names.render::<dyn Obj>().unwrap(), dyn_obj);
        assert_eq!(
            names.render::<&dyn Obj>().unwrap(),
            std::any::type_name::<&dyn Obj>()
        );
        assert_eq!(names.render::<Box<dyn Obj>>().unwrap(), format!("Box<{dyn_obj}>"));
        assert_eq!(
            names.render::<Vec<Box<dyn Obj>>>().unwrap(),
            format!("Vec<Box<{dyn_obj}>>")
        );
        assert_eq!(
            names.render::<dyn std::fmt::Debug>().unwrap(),
            std::any::type_name::<dyn std::fmt::Debug>()
        );
        assert_ne!(
            names.render::<dyn Obj>().unwrap(),
            names.render::<dyn Other>().unwrap()
        );
    }

    #[test]
    fn nesting_is_bounded_even_when_identical_markers_are_compared() {
        let names = Names::new::<DefaultNames>().unwrap();
        let depth = MAX_TYPE_DEPTH / 2;
        let valid = format!("{}u8{}", "Vec<".repeat(depth), ">".repeat(depth));
        assert!(names.equivalent(&valid, &valid).unwrap());
        let depth = MAX_TYPE_DEPTH * 4;
        for hostile in [
            format!("{}u8{}", "Vec<".repeat(depth), ">".repeat(depth)),
            format!("{}u8", "&".repeat(depth)),
            format!("{}u8{}", "(".repeat(depth), ",)".repeat(depth)),
        ] {
            let error = names.equivalent(&hostile, &hostile).unwrap_err();
            assert!(matches!(&error.kind, ErrorKind::InvalidTypeExpression(_)));
        }
    }

    #[test]
    fn lifetime_arguments_and_references_preserve_spelling_and_char_literals() {
        let names = Names::new::<NoNames>().unwrap();
        for expression in [
            "Borrowed<'_>",
            "Pair<'a, 'static>",
            "&'_ str",
            "&'static [u8]",
            "&'a mut Borrowed<'static>",
            r"Mixed<'a, 'a', '_', '\n', '\''>",
        ] {
            assert_eq!(
                Expression::parse(expression).unwrap().render(&names),
                expression
            );
            assert!(names.equivalent(expression, expression).unwrap());
        }
        assert!(!names.equivalent("Pair<'a,'_>", "Pair<'a, '_>").unwrap());
        assert!(!names.equivalent("Borrowed<'a>", "Borrowed<'a'>").unwrap());
        assert!(!names.equivalent("&'a str", "&'_ str").unwrap());
        assert!(
            names
                .equivalent("&'static (u8,u16)", "&'static (u8, u16)")
                .unwrap()
        );
        for malformed in ["Borrowed<'>", "Borrowed<'1>", "&'", "&'a' str"] {
            assert!(Expression::parse(malformed).is_err());
        }
    }

    #[test]
    fn compiler_reported_lifetimes_are_preserved_when_present() {
        struct BorrowedNames;
        impl NamePolicy for BorrowedNames {
            const SHORT_TYPES: &'static [ShortType] = &[ShortType::of::<Borrowed<'static>>()];
        }

        let names = Names::new::<NoNames>().unwrap();
        let full = type_name::<Borrowed<'_>>();
        assert_eq!(names.render::<Borrowed<'_>>().unwrap(), full);
        assert!(names.equivalent(full, full).unwrap());
        type Reference = &'static mut Borrowed<'static>;
        assert_eq!(
            names.render::<Reference>().unwrap(),
            type_name::<Reference>()
        );

        // The suffix may be empty or contain reported lifetime arguments.
        let path = constructor::<Borrowed<'_>>();
        assert_eq!(
            type_marker::<Borrowed<'_>, BorrowedNames>().unwrap(),
            format!("Borrowed{}", &full[path.len()..])
        );
    }

    #[test]
    fn truncated_utf8_boundary_inputs_never_panic() {
        for complete in [
            "app::Δ<(u8, Option<(u16, u32)>)>",
            r"G<'\u{10ffff}', true, -19>",
            "&mut [app::é<(u8,)>; 12]",
            "Borrowed<'_, &'a str, 'a', '\\n'>",
        ] {
            for (end, _) in complete.char_indices() {
                let _ = Expression::parse(&complete[..end]);
            }
        }
    }
}
