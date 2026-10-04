use crate::{
    FlatParserState, NonMaxUsize, State,
    Variant::{self, V2},
    error_new,
};

use super::{ErrorKind, Payload, Result, config::Context};

pub(super) enum Token<'de> {
    Metadata(Payload<'de>),
    Envelope(Payload<'de>),
}

fn formatting(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn metadata<'de>(tokens: &mut Vec<Token<'de>>, input: Payload<'de>, start: usize, end: usize) {
    let mut head = start;
    let mut tail = end;
    while head < tail && formatting(input.bytes()[head]) {
        head += 1;
    }
    while tail > head && formatting(input.bytes()[tail - 1]) {
        tail -= 1;
    }
    if head != tail {
        tokens.push(Token::Metadata(Payload::with_offset(
            &input.bytes()[head..tail],
            input.offset().saturating_add(head),
        )));
    }
}

/// Charge a bound before entering the fundamental limiter scanner. Single
/// scalar units cannot overlap internally, so short runs can be skipped while
/// accounting for the scanner's repeated attempts at every byte in the run.
fn limiter_work(context: &Context, input: &[u8], from: usize) -> Result<usize> {
    let pairs = context.config.limiter_pairs();
    let baseline: usize = pairs.iter().map(|pair| pair.limiter.len() + 2).sum();
    let limit = context.config.limits().max_delimiter_work;
    let available = context.remaining_work();
    let mut work = 0usize;
    let mut pos = from;
    'scan: while pos < input.len() {
        work = work.saturating_add(baseline);
        if work > available {
            return Err(context.limit("delimiter work", limit));
        }
        for pair in pairs {
            let unit = pair.limiter.as_slice();
            if input[pos..].starts_with(unit) {
                let mut end = pos;
                let mut repeats = 0usize;
                loop {
                    if work.saturating_add(end - pos).saturating_add(unit.len()) > available {
                        return Err(context.limit("delimiter work", limit));
                    }
                    if !input[end..].starts_with(unit) {
                        break;
                    }
                    repeats += 1;
                    end += unit.len();
                }
                let minimum = pair.least_repeat.map(|repeat| repeat.get()).unwrap_or(1);
                let pending = input[end..].is_empty() || unit.starts_with(&input[end..]);
                if repeats >= minimum || pending {
                    work =
                        work.saturating_add(repeats.saturating_add(1).saturating_mul(unit.len()));
                    if work > available {
                        return Err(context.limit("delimiter work", limit));
                    }
                    return Ok(work);
                }
                let attempts = repeats.saturating_mul(repeats.saturating_add(1)) / 2;
                work = work.saturating_add(attempts.saturating_mul(unit.len()));
                work = work.saturating_add((end - pos).saturating_mul(baseline));
                if work > available {
                    return Err(context.limit("delimiter work", limit));
                }
                pos = end;
                continue 'scan;
            }
            if unit.starts_with(&input[pos..]) {
                return Ok(work);
            }
        }
        pos += 1;
    }
    Ok(work)
}

fn delimiter_work(
    context: &Context,
    input: &[u8],
    from: usize,
    unit: &[u8],
    repeat: usize,
) -> Result<usize> {
    let width = repeat.saturating_mul(unit.len());
    let per_byte = width.saturating_add(4);
    let limit = context.config.limits().max_delimiter_work;
    let available = context.remaining_work();
    let mut work = 0usize;
    let mut pos = from;
    while width <= input.len().saturating_sub(pos) {
        if work.saturating_add(per_byte) > available {
            return Err(context.limit("delimiter work", limit));
        }
        if input[pos..].starts_with(unit) {
            let mut end = pos;
            let mut repeats = 0usize;
            loop {
                if work.saturating_add(end - pos).saturating_add(unit.len()) > available {
                    return Err(context.limit("delimiter work", limit));
                }
                if !input[end..].starts_with(unit) {
                    break;
                }
                repeats += 1;
                end += unit.len();
            }
            if repeats >= repeat && context.config.variant() == Variant::V1 {
                work = work.saturating_add(per_byte).saturating_add(end - pos);
                break;
            }
            work = work.saturating_add((end - pos).saturating_mul(per_byte));
            if repeats >= repeat {
                // V2 either confirms at the last matching window of this run
                // or remains pending at its end. No later bytes are scanned.
                break;
            }
            pos = end;
        } else {
            work = work.saturating_add(per_byte);
            pos += 1;
        }
        if work > available {
            return Err(context.limit("delimiter work", limit));
        }
    }
    // The fundamental scanner's trailing-prefix search is at most width-1
    // positions. Include it even for a confirmed frame as a conservative bound.
    work = work.saturating_add(
        input
            .len()
            .saturating_sub(pos)
            .min(width)
            .saturating_mul(per_byte),
    );
    if work > available {
        Err(context.limit("delimiter work", limit))
    } else {
        Ok(work)
    }
}

/// Tokenize exactly one bounded parsing level. There are no synthetic headers:
/// the free state machine emits real frames only. Its state is never reused on
/// different bytes, and a pending limiter/frame is an error even at EOF.
pub(super) fn parse<'de>(
    context: &mut Context,
    input: Payload<'de>,
    count_elements: bool,
) -> Result<Vec<Token<'de>>> {
    if count_elements {
        context
            .input(input.bytes().len())
            .map_err(|error| error.at(input.offset()))?;
    } else {
        context.output(input.bytes().len())?;
    }
    let mut state = FlatParserState::default(context.config.limiter_pairs()[0].clone());
    let mut cursor = 0;
    let mut tokens = Vec::new();
    loop {
        let work = limiter_work(context, input.bytes(), cursor)
            .map_err(|error| error.at(input.offset().saturating_add(cursor)))?;
        context
            .work(work)
            .map_err(|error| error.at(input.offset().saturating_add(cursor)))?;
        // SAFETY: the default state starts at zero; later iterations pass the
        // last archived frame's returned state on the identical bytes/profile.
        let new_state = unsafe {
            crate::parse_incremental(
                context.config.variant(),
                input.bytes(),
                context.config.limiter_pairs(),
                state.clone(),
            )
        };
        if new_state.state.eq(&state.state) {
            return Ok(tokens);
        }
        state = new_state;
        match state.state() {
            State::E { envelop } => {
                let head = envelop.head_offset().get();
                let repeat = envelop.repeat(state.limiter().limiter.len());
                context
                    .repeat(repeat)
                    .map_err(|error| error.at(input.offset().saturating_add(head)))?;
                let payload_head = envelop.payload_head_offset().get();
                let work = delimiter_work(
                    context,
                    input.bytes(),
                    payload_head,
                    &state.limiter().delimiter,
                    repeat,
                )
                .map_err(|error| error.at(input.offset().saturating_add(payload_head)))?;
                context
                    .work(work)
                    .map_err(|error| error.at(input.offset().saturating_add(payload_head)))?;
                // SAFETY: same returned live frame, same buffer and profile.
                let new_state = unsafe {
                    crate::parse_incremental(
                        context.config.variant(),
                        input.bytes(),
                        context.config.limiter_pairs(),
                        state.clone(),
                    )
                };
                let State::E { envelop } = new_state.state() else {
                    return Err(error_new!(ErrorKind::TruncatedEnvelope)
                        .at(input.offset().saturating_add(head)));
                };
                let tail = if let Some(tail) = envelop.tail_offset() {
                    tail
                } else {
                    if context.config.variant() == V2
                        && envelop.payload_head_offset.get() - envelop.head_offset.get()
                            == input.bytes().len() - envelop.payload_tail_offset.get()
                    {
                        unsafe { NonMaxUsize::new(input.bytes().len() - 1) }
                    } else {
                        return Err(error_new!(ErrorKind::TruncatedEnvelope)
                            .at(input.offset().saturating_add(head)));
                    }
                };
                metadata(&mut tokens, input, cursor, head);
                if count_elements {
                    context
                        .element()
                        .map_err(|error| error.at(input.offset().saturating_add(head)))?;
                }
                let payload_head = envelop.payload_head_offset().get();
                let payload_tail = envelop.payload_tail_offset().get();
                tokens.push(Token::Envelope(Payload::with_offset(
                    &input.bytes()[payload_head..payload_tail],
                    input.offset().saturating_add(payload_head),
                )));
                cursor = tail.get();
                // SAFETY: FULLY parsed, try push metadata with updated cursor as old tail of this Envelop and old head of this Envelop MUST cause head >= tail, a `slice index` panic.
                if new_state.state().eq(&state.state) {
                    continue;
                }
                state = new_state;
            }
            State::NonEnvelop { .. } => {
                metadata(&mut tokens, input, cursor, input.bytes().len());
                return Ok(tokens);
            }
            State::PartialLimiterSlice { head_offset } => {
                return Err(error_new!(ErrorKind::TruncatedEnvelope)
                    .at(input.offset().saturating_add(head_offset.get())));
            }
            State::Unknown => {
                return Err(error_new!(ErrorKind::TruncatedEnvelope)
                    .at(input.offset().saturating_add(cursor)));
            }
        }
    }
}

/// The longest delimiter-unit run strictly inside a payload. V2 permits a
/// trailing run: the closing slice is confirmed at the end of the whole run.
fn maximum_run(context: &mut Context, payload: &[u8], unit: &[u8]) -> Result<usize> {
    context.work(payload.len().saturating_mul(unit.len().saturating_add(1)))?;
    let mut pos = 0;
    let mut maximum = 0;
    while pos < payload.len() {
        if payload[pos..].starts_with(unit) {
            let mut end = pos;
            let mut repeats = 0;
            while payload[end..].starts_with(unit) {
                repeats += 1;
                end += unit.len();
            }
            if context.config.variant() == Variant::V1 || end != payload.len() {
                maximum = maximum.max(repeats);
            }
            pos = end;
        } else {
            pos += 1;
        }
    }
    Ok(maximum)
}

pub(super) fn frame(context: &mut Context, payload: &[u8]) -> Result<Vec<u8>> {
    context.output(payload.len())?;
    context.element()?;
    let pairs = context.config.limiter_pairs().to_vec();
    let mut limit_error = None;
    for pair in pairs {
        if payload.starts_with(&pair.limiter) {
            continue;
        }
        let maximum = maximum_run(context, payload, &pair.delimiter)?;
        let minimum = pair.least_repeat.map(|repeat| repeat.get()).unwrap_or(1);
        let repeat = minimum.max(maximum.saturating_add(1));
        if let Err(error) = context.repeat(repeat) {
            limit_error = Some(error);
            continue;
        }
        let overhead = repeat
            .saturating_mul(pair.limiter.len().saturating_add(pair.delimiter.len()))
            .saturating_add(1);
        let length = payload.len().saturating_add(overhead);
        if let Err(error) = context.output(length) {
            limit_error = Some(error);
            continue;
        }
        let mut candidate = Vec::with_capacity(length);
        for _ in 0..repeat {
            candidate.extend_from_slice(&pair.limiter);
        }
        let expected_head = candidate.len();
        candidate.extend_from_slice(payload);
        for _ in 0..repeat {
            candidate.extend_from_slice(&pair.delimiter);
        }
        // Units are single scalars, all distinct and none is ASCII formatting.
        // Newline cannot continue/prefix a delimiter or an opening limiter.
        // Every nested child gets its own confirmation inside the parent slice.
        match parse(context, Payload::new(&candidate), false) {
            Ok(tokens) => {
                if tokens.len() == 1
                    && matches!(&tokens[0], Token::Envelope(actual) if actual.offset() == expected_head && actual.bytes() == payload)
                {
                    return Ok(candidate);
                }
            }
            Err(error) if matches!(error.kind, ErrorKind::LimitExceeded { .. }) => {
                return Err(error);
            }
            Err(_) => {}
        }
    }
    Err(limit_error.unwrap_or_else(|| error_new!(ErrorKind::NoDelimiter)))
}

pub(super) fn marker(context: &mut Context, text: &str) -> Result<Vec<u8>> {
    context.output(text.len().saturating_add(1))?;
    let output = text.as_bytes().to_vec();
    match parse(context, Payload::new(&output), false) {
        Ok(tokens)
            if tokens.len() == 1
                && matches!(&tokens[0], Token::Metadata(actual) if actual.bytes() == text.as_bytes()) =>
        {
            Ok(output)
        }
        Err(error) if matches!(error.kind, ErrorKind::LimitExceeded { .. }) => Err(error),
        _ => Err(error_new!(ErrorKind::InvalidProfile(format!(
            "metadata {text:?} does not remain NonEnvelop under this profile"
        )))),
    }
}
