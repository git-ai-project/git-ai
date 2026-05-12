use {super::*, std::cmp::min};

// A line in pass 1 can be in one of the following states:
// - Visible: line does not get hidden.
// - Hidden line: line gets hidden because it has an empty end-of-line comment, but it is not part
//   of a hidden block of lines.
// - In-boundary: line does not get hidden, but its end-of-line comment starts a hidden block.
// - Hidden block: line is part of a hidden block, either because the hidden block was started at a
//   previous line or because this line is an `InBoundary` that has nothing besides an end-of-line
//   comment and whitespace.
// - Out-boundary: line gets hidden, but its end-of-line comment ends a hidden block.
pub(crate) struct Pass1<I> {
    lines:        I,
    total_length: usize,
    min_indent:   Option<usize>,
    anchor:       Option<Box<str>>,
    flags:        u8,
}
macro_rules! flags {
	($($constnm:ident $getnm:ident $setnm:ident = $val:literal),+ $(,)?) => {$(
		const $constnm: u8 = $val;
		#[inline(always)] fn $getnm(&self) -> bool { self.flags & (1 << Self::$constnm) != 0 }
		#[allow(dead_code)]
		#[inline(always)] fn $setnm(&mut self, val: bool) {
			self.flags = self.flags & !(1 << Self::$constnm) | ((val as u8) << Self::$constnm);
		}
	)+};
}
impl<I> Pass1<I> {
    pub fn new(lines: I, anchor: Option<Box<str>>, all_hidden: bool) -> Self {
        let flags = ((all_hidden as u8) << Self::ALL_HIDDEN)
            | ((anchor.is_none() as u8) << Self::HAD_ANCHOR);
        Pass1 { lines, total_length: 0, min_indent: None, anchor, flags }
    }
    pub fn total_length(&self) -> usize { self.total_length }
    pub fn min_indent(&self) -> usize { self.min_indent.unwrap_or(0) }
    pub fn anchor(&self) -> Option<&str> { self.anchor.as_deref() }

    flags! {
        // Hide all lines.
        ALL_HIDDEN all_hidden set_all_hidden = 0,
        // In a hidden block.
        HIDDEN_BLOCK hidden_block set_hidden_block = 1,
        // The requested anchor was seen previously. Always true if no anchor is requested.
        HAD_ANCHOR had_anchor set_had_anchor = 2,
        // Past the end of the requested anchor.
        ANCHOR_ENDED anchor_ended set_anchor_ended = 3,
    }

    /// Returns `true` if the requested anchor was not found.
    pub fn missing_anchor(&self) -> bool { !self.had_anchor() }
    /// Returns `true` if the requested anchor didn't have an `ANCHOR_END`.
    pub fn unclosed_anchor(&self) -> bool {
        self.anchor.is_some() && self.had_anchor() && !self.anchor_ended()
    }
}
impl<I: Iterator<Item = MResult<String>>> Iterator for Pass1<I> {
    type Item = MResult<(String, bool)>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut linebuf = match self.lines.next()? {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            remove_trailing_ws(&mut linebuf);
            let trim_start_off = linebuf.len() - linebuf.trim_start().len();

            let comment_only =
                linebuf.as_bytes()[trim_start_off..].get(0..2).unwrap_or(b"//") == b"//";
            let mut visible = self.had_anchor()
                && !(self.all_hidden() || self.hidden_block() || self.anchor_ended());
            let mut hide_if_empty = false;
            while let Some(slashslash) = last_slashslash(&linebuf) {
                let had_len = linebuf.len();
                match linebuf.as_bytes().get(slashslash + 2) {
                    Some(&b'{') => {
                        self.set_hidden_block(true);
                        trunc_and_remove_trailing_ws(&mut linebuf, slashslash);
                        hide_if_empty = true;
                    }
                    Some(&b'}') => {
                        self.set_hidden_block(false);
                        trunc_and_remove_trailing_ws(&mut linebuf, slashslash);
                        visible = false;
                    }
                    None => {
                        if !comment_only {
                            trunc_and_remove_trailing_ws(&mut linebuf, slashslash);
                            visible = false;
                        }
                    }
                    _ => {
                        let s = linebuf[slashslash + 2..].trim_start();
                        if let Some(s) = s.strip_prefix("ANCHOR") {
                            if let Some(mut s) = s.strip_prefix(':') {
                                s = s.trim_start();
                                if self.anchor.as_deref() == Some(s) {
                                    self.set_had_anchor(true);
                                }
                                trunc_and_remove_trailing_ws(&mut linebuf, slashslash);
                            } else if let Some(mut s) = s.strip_prefix("_END:") {
                                s = s.trim_start();
                                if self.anchor.as_deref() == Some(s) {
                                    self.set_anchor_ended(true);
                                    hide_if_empty = true;
                                }
                                trunc_and_remove_trailing_ws(&mut linebuf, slashslash);
                            }
                        }
                    }
                }
                if linebuf.len() == had_len {
                    break;
                }
            }

            if (!visible || hide_if_empty) && linebuf.is_empty() {
                // Hidden empty lines are lines we don't need to emit at all.
                continue;
            }

            // Empty lines have indeterminate indentation, meaning that a dedent that happens at a blank
            // line is not really a dedent.
            let (indent, indent_bytes) = indent_of(&linebuf);
            if visible && !eat_start(&linebuf, trim_start_off).is_empty() {
                self.min_indent = Some(min(self.min_indent.unwrap_or(usize::MAX), indent));
            }
            self.total_length += linebuf.len() + indent - indent_bytes;

            if !visible {
                self.total_length += 2;
            }

            break Some(Ok((linebuf, visible)));
        }
    }
}

/// Returns the indent of the string and the number of indent bytes that indent is comprised of.
pub(crate) fn indent_of(s: &str) -> (usize, usize) {
    // Rustdoc always uses a tapstop value of 4, as of Rust 1.94.
    const TAB_WIDTH: usize = 4;
    let mut ctr = 0;
    s.chars()
        .map_while(|c| {
            Some(match c {
                ' ' => {
                    ctr = (ctr + 1) % TAB_WIDTH;
                    1
                }
                '\t' => {
                    let ind = TAB_WIDTH - ctr;
                    ctr = 0;
                    ind
                }
                _ => return None,
            })
        })
        .fold((0, 0), |(ind, cnt), incr| (ind + incr, cnt + 1))
}

/// Returns the byte index to the first slash character of the last occurrence of `//` in the
/// string, searching backwards and stopping the search when ASCII punctuation other than `/`, `{`
/// and `}` is encountered.
fn last_slashslash(s: &str) -> Option<usize> {
    let s = s.as_bytes();
    s.iter()
        .enumerate()
        .rev()
        .take_while(|&(_, &c)| {
            !c.is_ascii_punctuation()
                || c == b'/'
                || c == b'{'
                || c == b'}'
                || c == b':'
                || c == b'_'
                || c == b'-'
        })
        .find(|(i, &c)| c == b'/' && i.checked_sub(1).and_then(|i| s.get(i)) == Some(&b'/'))
        .map(|(p, _)| p - 1) // offset to first slash
}

fn eat_start(s: &str, amount: usize) -> &str { &s[std::cmp::min(amount, s.len())..] }
