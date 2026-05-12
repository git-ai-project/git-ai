#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]
extern crate proc_macro;

mod args;
mod logic;
mod tokenmanip;
mod imports {
    pub(crate) use proc_macro::{
        Delimiter, Group, Ident as I, Literal, Punct, Spacing, Span, TokenStream as TS,
        TokenTree as TT,
    };
}

use {
    crate::args::Arg,
    imports::*,
    logic::*,
    std::{
        borrow::Cow,
        fs::File,
        io::{BufRead, BufReader},
        path::{Path, PathBuf},
    },
    tokenmanip::*,
};

type MResult<T = TS> = Result<T, Error>;

struct Error {
    msg:  Cow<'static, str>,
    span: Span,
}
impl Error {
    fn new(msg: impl Into<Cow<'static, str>>, span: Span) -> Self {
        Self { msg: msg.into(), span }
    }
}
#[inline(always)]
fn err<T>(msg: impl Into<Cow<'static, str>>, span: Span) -> MResult<T> {
    Err(Error::new(msg, span))
}

/// Includes a documentation test from a separate file, **without** inserting the surrounding
/// \`\`\` markers.
///
/// See the [crate-level documentation](crate) for more.
#[proc_macro]
pub fn include_doctest(input: TS) -> TS { macro_main(input).unwrap_or_else(compile_error) }

struct Input {
    filename:      PathBuf,
    filename_span: Span,
    hidden:        bool,
    region:        String,
}

fn parse_input(input: TS) -> MResult<Input> {
    let mut input = args::comma_separated(input);

    let Some(filename) = input.next() else {
        return err("expected filename", Span::call_site());
    };

    let mut result = match filename? {
        Arg::Path(l) => Input {
            filename:      PathBuf::from(parse_literal(&l)?),
            filename_span: l.span(),
            hidden:        false,
            region:        String::new(),
        },
        other => return err("expected filename as first argument", other.left_span()),
    };

    for arg in input {
        match arg? {
            Arg::Key(id, opt) if id.to_string() == "hidden" => {
                if result.hidden {
                    return err("duplicate `hidden` option", id.span());
                }
                if let Some(opt) = opt {
                    return err("`hidden` is not a key-value option", opt.span());
                }
                result.hidden = true;
            }
            Arg::Key(id, v) if id.to_string() == "region" => {
                if !result.region.is_empty() {
                    return err("duplicate `region` key", id.span());
                }
                match v {
                    Some(TT::Literal(l)) => result.region = parse_literal(&l)?,
                    Some(tt) => return err("region name must be a string", tt.span()),
                    None => return err("`region` requires a region name", id.span()),
                }
            }
            Arg::Key(id, ..) => {
                return err("no such option", id.span());
            }
            Arg::Path(l) => {
                return err("doctest path may not be specified multiple times", l.span())
            }
        }
    }

    Ok(result)
}

fn convert_path(path: &Path) -> MResult<Cow<'_, Path>> {
    if path.is_absolute() {
        return Ok(Cow::Borrowed(path));
    }
    let mut pb = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from).ok_or_else(|| {
        Error::new("the CARGO_MANIFEST_DIR environment variable is not set", Span::call_site())
    })?;
    pb.push(path);
    Ok(Cow::Owned(pb))
}

fn macro_main(input: TS) -> MResult {
    let input = parse_input(input)?;
    let path = convert_path(&input.filename)?;

    let fln = input.filename.display();
    let ioe = |m, e| Error::new(format!("{m} (file {fln}): {e}"), input.filename_span);
    let file = File::open(path).map_err(|e| ioe("could not open", e))?;

    let lines = BufReader::new(file).lines().map(|rslt| rslt.map_err(|e| ioe("read failed", e)));

    let anchor = (!input.region.is_empty()).then(move || Box::from(input.region));
    let mut pass1 = Pass1::new(lines, anchor, input.hidden);
    let mut lines_pass2 = Vec::with_capacity(std::cmp::max(256, pass1.total_length() / 50));
    for l in &mut pass1 {
        lines_pass2.push(l?);
    }

    if pass1.missing_anchor() {
        return err(
            format!("anchor `{}` not found", pass1.anchor().unwrap(),),
            input.filename_span,
        );
    }
    if pass1.unclosed_anchor() {
        return err("missing ANCHOR_END statement", input.filename_span);
    }

    let mut docstring = String::with_capacity(pass1.total_length());
    let dedent = pass1.min_indent();

    let mut writing = false;
    for (line, visible) in lines_pass2 {
        if visible {
            let line_trimmed = line.trim_start();
            if !line_trimmed.is_empty() {
                writing = true;
            }
            if !writing {
                continue;
            }
            let indent = indent_of(&line).0;
            for _ in 0..indent.saturating_sub(dedent) {
                docstring.push(' ');
            }
            docstring.push_str(line_trimmed);
        } else {
            writing = true;
            docstring.push_str("# ");
            docstring.push_str(&line);
        }
        docstring.push('\n');
    }

    remove_trailing_ws(&mut docstring);

    Ok(TT::Literal(Literal::string(&docstring)).into())
}

/// Truncates the given string to the given length and removes all trailing whitespace following
/// this truncation.
fn trunc_and_remove_trailing_ws(s: &mut String, trunc: usize) {
    s.truncate(trunc);
    remove_trailing_ws(s);
}
/// Removes all trailing whitespace in the string in place.
fn remove_trailing_ws(s: &mut String) { s.truncate(s.trim_end().len()) }
