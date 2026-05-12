//! Argument parsing in the style of built-in attributes.

use super::{MResult, *};

/// Syntactically valid macro argument.
pub(crate) enum Arg {
    Path(Literal),
    Key(I, Option<TT>),
}

impl Arg {
    /// Returns the span of whatever is to the left of `=`, if there is an `=`.
    pub(crate) fn left_span(&self) -> Span {
        match self {
            Arg::Path(l) => l.span(),
            Arg::Key(id, ..) => id.span(),
        }
    }
}

pub(crate) fn comma_separated(ts: TS) -> impl Iterator<Item = MResult<Arg>> {
    let mut ts = ts.into_iter().map(|t| match t {
        TT::Punct(p) if p.as_char() == ',' => Sep::S(p.span()),
        other => Sep::T(other),
    });
    std::iter::from_fn(move || {
        Some('r: {
            let left = match ts.next()? {
                Sep::T(t) => t,
                Sep::S(span) => break 'r err("expected argument", span),
            };
            let mid_span = match ts.next() {
                Some(Sep::S(..)) | None => {
                    break 'r match left {
                        TT::Literal(l) => Ok(Arg::Path(l)),
                        TT::Ident(i) => Ok(Arg::Key(i, None)),
                        other => err("expected identifier or literal", other.span()),
                    }
                }
                Some(Sep::T(TT::Punct(p))) if p.as_char() == '=' => p.span(),
                Some(Sep::T(t)) => break 'r err("expected `=` or `,`", t.span()),
            };
            let left = match left {
                TT::Ident(i) => i,
                other => break 'r err("key must be an identifier", other.span()),
            };
            let expkey = |s| err("expected value for key", s);
            let val = match ts.next() {
                None => break 'r expkey(mid_span),
                Some(Sep::S(span)) => break 'r expkey(span),
                Some(Sep::T(val)) => val,
            };
            if let Some(Sep::T(t)) = ts.next() {
                break 'r err("unexpected token following key value", t.span());
            }
            Ok(Arg::Key(left, Some(val)))
        })
    })
}
#[derive(Debug)]
enum Sep {
    T(TT),
    S(Span),
}
