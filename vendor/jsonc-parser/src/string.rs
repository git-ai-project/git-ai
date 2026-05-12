use std::borrow::Cow;

pub struct ParseStringError {
  pub byte_index: usize,
  pub kind: ParseStringErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ParseStringErrorKind {
  InvalidEscapeInSingleQuoteString,
  InvalidEscapeInDoubleQuoteString,
  ExpectedFourHexDigits,
  InvalidUnicodeEscapeSequence(String),
  InvalidEscape,
  UnterminatedStringLiteral,
}

impl std::error::Error for ParseStringErrorKind {}

impl std::fmt::Display for ParseStringErrorKind {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      ParseStringErrorKind::InvalidEscapeInSingleQuoteString => {
        write!(f, "Invalid escape in single quote string")
      }
      ParseStringErrorKind::InvalidEscapeInDoubleQuoteString => {
        write!(f, "Invalid escape in double quote string")
      }
      ParseStringErrorKind::ExpectedFourHexDigits => {
        write!(f, "Expected four hex digits")
      }
      ParseStringErrorKind::InvalidUnicodeEscapeSequence(value) => {
        write!(
          f,
          "Invalid unicode escape sequence. '{}' is not a valid UTF8 character",
          value
        )
      }
      ParseStringErrorKind::InvalidEscape => {
        write!(f, "Invalid escape")
      }
      ParseStringErrorKind::UnterminatedStringLiteral => {
        write!(f, "Unterminated string literal")
      }
    }
  }
}

pub trait CharProvider<'a> {
  fn current_char(&mut self) -> Option<char>;
  fn byte_index(&self) -> usize;
  fn move_next_char(&mut self) -> Option<char>;
  fn text(&self) -> &'a str;
}

#[cfg(feature = "cst")]
pub fn parse_string(text: &str) -> Result<Cow<'_, str>, ParseStringError> {
  struct StringCharProvider<'a> {
    text: &'a str,
    byte_index: usize,
    current_char: Option<char>,
    chars: std::str::Chars<'a>,
  }

  impl<'a> CharProvider<'a> for StringCharProvider<'a> {
    fn current_char(&mut self) -> Option<char> {
      self.current_char
    }

    fn byte_index(&self) -> usize {
      self.byte_index
    }

    fn move_next_char(&mut self) -> Option<char> {
      if let Some(current_char) = self.current_char {
        self.byte_index += current_char.len_utf8();
      }
      self.current_char = self.chars.next();
      self.current_char
    }

    fn text(&self) -> &'a str {
      self.text
    }
  }

  let mut chars = text.chars();
  let mut provider = StringCharProvider {
    text,
    byte_index: 0,
    current_char: chars.next(),
    chars,
  };

  parse_string_with_char_provider(&mut provider)
}

pub fn parse_string_with_char_provider<'a, T: CharProvider<'a>>(
  chars: &mut T,
) -> Result<Cow<'a, str>, ParseStringError> {
  debug_assert!(
    chars.current_char() == Some('\'') || chars.current_char() == Some('"'),
    "Expected \", was {:?}",
    chars.current_char()
  );
  let is_double_quote = chars.current_char() == Some('"');
  let mut last_start_byte_index = chars.byte_index() + 1;
  let mut text: Option<String> = None;
  let mut last_was_backslash = false;
  let mut found_end_string = false;
  let token_start = chars.byte_index();

  while let Some(current_char) = chars.move_next_char() {
    if last_was_backslash {
      let escape_start = chars.byte_index() - 1; // -1 for backslash
      match current_char {
        '"' | '\'' | '\\' | '/' | 'b' | 'f' | 'u' | 'r' | 'n' | 't' => {
          if current_char == '"' {
            if !is_double_quote {
              return Err(ParseStringError {
                byte_index: escape_start,
                kind: ParseStringErrorKind::InvalidEscapeInSingleQuoteString,
              });
            }
          } else if current_char == '\'' && is_double_quote {
            return Err(ParseStringError {
              byte_index: escape_start,
              kind: ParseStringErrorKind::InvalidEscapeInDoubleQuoteString,
            });
          }

          let previous_text = &chars.text()[last_start_byte_index..escape_start];
          if text.is_none() {
            text = Some(String::new());
          }
          let text = text.as_mut().unwrap();
          text.push_str(previous_text);
          if current_char == 'u' {
            let hex_char = parse_hex_char(chars).map_err(|kind| ParseStringError {
              byte_index: escape_start,
              kind,
            })?;
            text.push(hex_char);
            last_start_byte_index = chars.byte_index() + chars.current_char().map(|c| c.len_utf8()).unwrap_or(0);
          } else {
            text.push(match current_char {
              'b' => '\u{08}',
              'f' => '\u{0C}',
              't' => '\t',
              'r' => '\r',
              'n' => '\n',
              _ => current_char,
            });
            last_start_byte_index = chars.byte_index() + current_char.len_utf8();
          }
        }
        _ => {
          return Err(ParseStringError {
            byte_index: escape_start,
            kind: ParseStringErrorKind::InvalidEscape,
          });
        }
      }
      last_was_backslash = false;
    } else if is_double_quote && current_char == '"' || !is_double_quote && current_char == '\'' {
      found_end_string = true;
      break;
    } else {
      last_was_backslash = current_char == '\\';
    }
  }

  if found_end_string {
    chars.move_next_char();
    let final_segment = &chars.text()[last_start_byte_index..chars.byte_index() - 1];
    Ok(match text {
      Some(mut text) => {
        text.push_str(final_segment);
        Cow::Owned(text)
      }
      None => Cow::Borrowed(final_segment),
    })
  } else {
    Err(ParseStringError {
      byte_index: token_start,
      kind: ParseStringErrorKind::UnterminatedStringLiteral,
    })
  }
}

fn read_four_hex_digits<'a, T: CharProvider<'a>>(
  chars: &mut T,
  buf: &mut [u8; 4],
) -> Result<u32, ParseStringErrorKind> {
  for slot in buf.iter_mut() {
    match chars.move_next_char() {
      Some(c) if c.is_ascii_hexdigit() => *slot = c as u8,
      _ => return Err(ParseStringErrorKind::ExpectedFourHexDigits),
    }
  }
  // safety: buf contains only ASCII hex digits
  let hex_str = std::str::from_utf8(buf).unwrap();
  u32::from_str_radix(hex_str, 16).map_err(|_| ParseStringErrorKind::InvalidUnicodeEscapeSequence(hex_str.to_string()))
}

fn hex_buf_to_str(buf: &[u8; 4]) -> String {
  std::str::from_utf8(buf).unwrap().to_string()
}

fn parse_hex_char<'a, T: CharProvider<'a>>(chars: &mut T) -> Result<char, ParseStringErrorKind> {
  let mut buf1 = [0u8; 4];
  let hex_value = read_four_hex_digits(chars, &mut buf1)?;

  // check if this is a high surrogate (0xD800-0xDBFF)
  let hex_char = if (0xD800..=0xDBFF).contains(&hex_value) {
    // high surrogate - must be followed by low surrogate (\uXXXX)
    if chars.move_next_char() != Some('\\') {
      return Err(ParseStringErrorKind::InvalidUnicodeEscapeSequence(format!(
        "{} (unpaired high surrogate)",
        hex_buf_to_str(&buf1)
      )));
    }

    if chars.move_next_char() != Some('u') {
      return Err(ParseStringErrorKind::InvalidUnicodeEscapeSequence(format!(
        "{} (unpaired high surrogate)",
        hex_buf_to_str(&buf1)
      )));
    }

    // parse the second \uXXXX
    let mut buf2 = [0u8; 4];
    let hex_value2 = read_four_hex_digits(chars, &mut buf2)?;

    // verify it's a low surrogate (0xDC00-0xDFFF)
    if !(0xDC00..=0xDFFF).contains(&hex_value2) {
      return Err(ParseStringErrorKind::InvalidUnicodeEscapeSequence(format!(
        "{} (high surrogate not followed by low surrogate)",
        hex_buf_to_str(&buf1)
      )));
    }

    // combine surrogate pair using RFC 8259 formula
    let code_point = ((hex_value - 0xD800) * 0x400) + (hex_value2 - 0xDC00) + 0x10000;

    match std::char::from_u32(code_point) {
      Some(c) => c,
      None => {
        return Err(ParseStringErrorKind::InvalidUnicodeEscapeSequence(format!(
          "{}\\u{} (invalid surrogate pair)",
          hex_buf_to_str(&buf1),
          hex_buf_to_str(&buf2)
        )));
      }
    }
  } else if (0xDC00..=0xDFFF).contains(&hex_value) {
    // low surrogate without high surrogate
    return Err(ParseStringErrorKind::InvalidUnicodeEscapeSequence(format!(
      "{} (unpaired low surrogate)",
      hex_buf_to_str(&buf1)
    )));
  } else {
    // normal unicode escape
    match std::char::from_u32(hex_value) {
      Some(hex_char) => hex_char,
      None => {
        return Err(ParseStringErrorKind::InvalidUnicodeEscapeSequence(hex_buf_to_str(
          &buf1,
        )));
      }
    }
  };
  Ok(hex_char)
}
