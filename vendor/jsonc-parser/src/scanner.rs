use std::borrow::Cow;

use crate::string::CharProvider;

use super::common::Range;
use super::errors::*;
use super::tokens::Token;

/// Converts text into a stream of tokens.
pub struct Scanner<'a> {
  byte_index: usize,
  token_start: usize,
  bytes: &'a [u8],
  current_token: Option<Token<'a>>,
  file_text: &'a str,
  allow_single_quoted_strings: bool,
  allow_hexadecimal_numbers: bool,
  allow_unary_plus_numbers: bool,
}

/// Options for the scanner.
#[derive(Debug)]
pub struct ScannerOptions {
  /// Allow single-quoted strings (defaults to `true`).
  pub allow_single_quoted_strings: bool,
  /// Allow hexadecimal numbers like 0xFF (defaults to `true`).
  pub allow_hexadecimal_numbers: bool,
  /// Allow unary plus sign on numbers like +42 (defaults to `true`).
  pub allow_unary_plus_numbers: bool,
}

impl Default for ScannerOptions {
  fn default() -> Self {
    Self {
      allow_single_quoted_strings: true,
      allow_hexadecimal_numbers: true,
      allow_unary_plus_numbers: true,
    }
  }
}

impl<'a> Scanner<'a> {
  /// Creates a new scanner with specific options.
  pub fn new(file_text: &'a str, options: &ScannerOptions) -> Scanner<'a> {
    Scanner {
      byte_index: 0,
      token_start: 0,
      bytes: file_text.as_bytes(),
      current_token: None,
      file_text,
      allow_single_quoted_strings: options.allow_single_quoted_strings,
      allow_hexadecimal_numbers: options.allow_hexadecimal_numbers,
      allow_unary_plus_numbers: options.allow_unary_plus_numbers,
    }
  }

  pub fn file_text(&self) -> &str {
    self.file_text
  }

  /// Moves to and returns the next token.
  pub fn scan(&mut self) -> Result<Option<Token<'a>>, ParseError> {
    self.skip_whitespace();
    self.token_start = self.byte_index;
    if let Some(&b) = self.bytes.get(self.byte_index) {
      let token_result = match b {
        b'{' => {
          self.byte_index += 1;
          Ok(Token::OpenBrace)
        }
        b'}' => {
          self.byte_index += 1;
          Ok(Token::CloseBrace)
        }
        b'[' => {
          self.byte_index += 1;
          Ok(Token::OpenBracket)
        }
        b']' => {
          self.byte_index += 1;
          Ok(Token::CloseBracket)
        }
        b',' => {
          self.byte_index += 1;
          Ok(Token::Comma)
        }
        b':' => {
          self.byte_index += 1;
          Ok(Token::Colon)
        }
        b'\'' => {
          if self.allow_single_quoted_strings {
            self.parse_string()
          } else {
            Err(self.create_error_for_current_token(ParseErrorKind::SingleQuotedStringsNotAllowed))
          }
        }
        b'"' => self.parse_string(),
        b'/' => match self.bytes.get(self.byte_index + 1) {
          Some(b'/') => Ok(self.parse_comment_line()),
          Some(b'*') => self.parse_comment_block(),
          _ => Err(self.create_error_for_current_token(ParseErrorKind::UnexpectedToken)),
        },
        b'-' | b'+' | b'0'..=b'9' => self.parse_number(),
        b't' if self.try_move_word("true") => Ok(Token::Boolean(true)),
        b'f' if self.try_move_word("false") => Ok(Token::Boolean(false)),
        b'n' if self.try_move_word("null") => Ok(Token::Null),
        _ => self.parse_word(),
      };
      match token_result {
        Ok(token) => {
          self.current_token = Some(token.clone());
          Ok(Some(token))
        }
        Err(err) => Err(err),
      }
    } else {
      self.current_token = None;
      Ok(None)
    }
  }

  /// Gets the start position of the token.
  pub fn token_start(&self) -> usize {
    self.token_start
  }

  /// Gets the end position of the token.
  pub fn token_end(&self) -> usize {
    self.byte_index
  }

  /// Gets the current token.
  pub fn token(&self) -> Option<Token<'a>> {
    self.current_token.as_ref().map(|x| x.to_owned())
  }

  pub(super) fn create_error_for_current_token(&self, kind: ParseErrorKind) -> ParseError {
    let end = if self.byte_index > self.token_start {
      // token was fully scanned — use the exact token end
      self.byte_index
    } else if let Some(c) = self.file_text[self.byte_index..].chars().next() {
      // scanner hasn't advanced past token_start — cover the current character
      self.byte_index + c.len_utf8()
    } else {
      self.file_text.len()
    };
    let range = Range {
      start: self.token_start,
      end,
    };
    self.create_error_for_range(range, kind)
  }

  pub(super) fn create_error_for_current_char(&self, kind: ParseErrorKind) -> ParseError {
    self.create_error_for_start(self.byte_index, kind)
  }

  pub(super) fn create_error_for_start(&self, start: usize, kind: ParseErrorKind) -> ParseError {
    let range = Range {
      start,
      end: if let Some(c) = self.file_text[self.byte_index..].chars().next() {
        self.byte_index + c.len_utf8()
      } else {
        self.file_text.len()
      },
    };
    self.create_error_for_range(range, kind)
  }

  pub(super) fn create_error_for_range(&self, range: Range, kind: ParseErrorKind) -> ParseError {
    ParseError::new(range, kind, self.file_text)
  }

  fn parse_string(&mut self) -> Result<Token<'a>, ParseError> {
    let quote = self.bytes[self.byte_index];
    let start = self.byte_index + 1;

    // fast path: scan for closing quote or backslash byte-by-byte.
    // this is safe because quote (0x22/0x27) and backslash (0x5C) are ASCII
    // and can never appear as continuation bytes in multi-byte UTF-8 sequences.
    let mut i = start;
    while i < self.bytes.len() {
      let b = self.bytes[i];
      if b == quote {
        // found closing quote with no escapes
        let s = &self.file_text[start..i];
        self.byte_index = i + 1;
        return Ok(Token::String(Cow::Borrowed(s)));
      }
      if b == b'\\' {
        break;
      }
      i += 1;
    }

    // slow path: handle escape sequences via CharProvider
    crate::string::parse_string_with_char_provider(self)
      .map(Token::String)
      // todo(dsherret): don't convert the error kind to a string here
      .map_err(|err| self.create_error_for_start(err.byte_index, ParseErrorKind::String(err.kind)))
  }

  fn parse_number(&mut self) -> Result<Token<'a>, ParseError> {
    let start_byte_index = self.byte_index;

    // handle unary plus and unary minus
    match self.bytes.get(self.byte_index) {
      Some(b'+') => {
        if !self.allow_unary_plus_numbers {
          return Err(self.create_error_for_current_token(ParseErrorKind::UnaryPlusNumbersNotAllowed));
        }
        self.byte_index += 1;
      }
      Some(b'-') => {
        self.byte_index += 1;
      }
      _ => {}
    }

    match self.bytes.get(self.byte_index) {
      Some(b'0') => {
        self.byte_index += 1;

        // check for hexadecimal literal (0x or 0X)
        if matches!(self.bytes.get(self.byte_index), Some(b'x' | b'X')) {
          if !self.allow_hexadecimal_numbers {
            return Err(self.create_error_for_current_token(ParseErrorKind::HexadecimalNumbersNotAllowed));
          }

          self.byte_index += 1;

          // must have at least one hex digit
          if !matches!(self.bytes.get(self.byte_index), Some(b) if b.is_ascii_hexdigit()) {
            return Err(self.create_error_for_current_char(ParseErrorKind::ExpectedDigit));
          }

          while matches!(self.bytes.get(self.byte_index), Some(b) if b.is_ascii_hexdigit()) {
            self.byte_index += 1;
          }

          return Ok(Token::Number(&self.file_text[start_byte_index..self.byte_index]));
        }
      }
      Some(b'1'..=b'9') => {
        self.byte_index += 1;
        while matches!(self.bytes.get(self.byte_index), Some(b'0'..=b'9')) {
          self.byte_index += 1;
        }
      }
      _ => {
        return Err(self.create_error_for_current_char(ParseErrorKind::ExpectedDigitFollowingNegativeSign));
      }
    }

    if self.bytes.get(self.byte_index) == Some(&b'.') {
      self.byte_index += 1;

      if !matches!(self.bytes.get(self.byte_index), Some(b'0'..=b'9')) {
        return Err(self.create_error_for_current_char(ParseErrorKind::ExpectedDigit));
      }

      while matches!(self.bytes.get(self.byte_index), Some(b'0'..=b'9')) {
        self.byte_index += 1;
      }
    }

    if matches!(self.bytes.get(self.byte_index), Some(b'e' | b'E')) {
      self.byte_index += 1;

      match self.bytes.get(self.byte_index) {
        Some(b'-' | b'+') => {
          self.byte_index += 1;
          if !matches!(self.bytes.get(self.byte_index), Some(b'0'..=b'9')) {
            return Err(self.create_error_for_current_char(ParseErrorKind::ExpectedDigit));
          }
        }
        Some(b'0'..=b'9') => {}
        _ => {
          return Err(self.create_error_for_current_char(ParseErrorKind::ExpectedPlusMinusOrDigitInNumberLiteral));
        }
      }

      while matches!(self.bytes.get(self.byte_index), Some(b'0'..=b'9')) {
        self.byte_index += 1;
      }
    }

    Ok(Token::Number(&self.file_text[start_byte_index..self.byte_index]))
  }

  fn parse_comment_line(&mut self) -> Token<'a> {
    debug_assert!(self.bytes[self.byte_index] == b'/');
    self.byte_index += 1;
    debug_assert!(self.bytes[self.byte_index] == b'/');
    let start_byte_index = self.byte_index + 1;
    self.byte_index += 1;

    // scan byte-by-byte for newline; \n (0x0A) and \r (0x0D) are ASCII
    // and can never appear as UTF-8 continuation bytes
    while let Some(&b) = self.bytes.get(self.byte_index) {
      if b == b'\n' {
        break;
      }
      if b == b'\r' && self.bytes.get(self.byte_index + 1) == Some(&b'\n') {
        break;
      }
      self.byte_index += 1;
    }

    Token::CommentLine(&self.file_text[start_byte_index..self.byte_index])
  }

  fn parse_comment_block(&mut self) -> Result<Token<'a>, ParseError> {
    debug_assert!(self.bytes[self.byte_index] == b'/');
    self.byte_index += 1;
    debug_assert!(self.bytes[self.byte_index] == b'*');
    let start_byte_index = self.byte_index + 1;
    self.byte_index += 1;

    // scan byte-by-byte for */; both are ASCII and safe to scan through UTF-8
    loop {
      match self.bytes.get(self.byte_index) {
        Some(&b'*') if self.bytes.get(self.byte_index + 1) == Some(&b'/') => {
          let end_byte_index = self.byte_index;
          self.byte_index += 2;
          return Ok(Token::CommentBlock(&self.file_text[start_byte_index..end_byte_index]));
        }
        Some(_) => self.byte_index += 1,
        None => return Err(self.create_error_for_current_token(ParseErrorKind::UnterminatedCommentBlock)),
      }
    }
  }

  fn skip_whitespace(&mut self) {
    while let Some(&b) = self.bytes.get(self.byte_index) {
      if b <= b' ' {
        match b {
          b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => {
            self.byte_index += 1;
            continue;
          }
          _ => break,
        }
      } else if b >= 0x80 {
        // handle non-ASCII unicode whitespace
        let c = self.file_text[self.byte_index..].chars().next().unwrap();
        if c.is_whitespace() {
          self.byte_index += c.len_utf8();
          continue;
        }
        break;
      } else {
        break;
      }
    }
  }

  fn try_move_word(&mut self, text: &str) -> bool {
    let text_bytes = text.as_bytes();
    let end = self.byte_index + text_bytes.len();
    if end > self.bytes.len() {
      return false;
    }
    if &self.bytes[self.byte_index..end] != text_bytes {
      return false;
    }
    // ensure the word is not followed by an alphanumeric character
    if let Some(&next_byte) = self.bytes.get(end) {
      if next_byte.is_ascii_alphanumeric() {
        return false;
      }
      // check non-ASCII alphanumeric
      if next_byte >= 0x80
        && let Some(c) = self.file_text[end..].chars().next()
        && c.is_alphanumeric()
      {
        return false;
      }
    }
    self.byte_index = end;
    true
  }

  fn parse_word(&mut self) -> Result<Token<'a>, ParseError> {
    let start_byte_index = self.byte_index;

    while self.byte_index < self.bytes.len() {
      let b = self.bytes[self.byte_index];
      if b < 0x80 {
        // ASCII fast path
        if b.is_ascii_whitespace() || b == b':' {
          break;
        }
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
          self.byte_index += 1;
        } else {
          return Err(self.create_error_for_current_token(ParseErrorKind::UnexpectedToken));
        }
      } else {
        // non-ASCII: decode char
        let c = self.file_text[self.byte_index..].chars().next().unwrap();
        if c.is_whitespace() {
          break;
        }
        if c.is_alphanumeric() {
          self.byte_index += c.len_utf8();
        } else {
          return Err(self.create_error_for_current_token(ParseErrorKind::UnexpectedToken));
        }
      }
    }

    if self.byte_index == start_byte_index {
      return Err(self.create_error_for_current_token(ParseErrorKind::UnexpectedToken));
    }

    Ok(Token::Word(&self.file_text[start_byte_index..self.byte_index]))
  }

  fn current_char(&self) -> Option<char> {
    let &b = self.bytes.get(self.byte_index)?;
    if b < 0x80 {
      Some(b as char)
    } else {
      self.file_text[self.byte_index..].chars().next()
    }
  }

  fn move_next_char(&mut self) -> Option<char> {
    if self.byte_index >= self.bytes.len() {
      return None;
    }
    let b = self.bytes[self.byte_index];
    if b < 0x80 {
      self.byte_index += 1;
    } else {
      let c = self.file_text[self.byte_index..].chars().next().unwrap();
      self.byte_index += c.len_utf8();
    }
    self.current_char()
  }
}

impl<'a> CharProvider<'a> for Scanner<'a> {
  fn current_char(&mut self) -> Option<char> {
    Scanner::current_char(self)
  }

  fn move_next_char(&mut self) -> Option<char> {
    Scanner::move_next_char(self)
  }

  fn byte_index(&self) -> usize {
    self.byte_index
  }

  fn text(&self) -> &'a str {
    self.file_text
  }
}

#[cfg(test)]
mod tests {
  use std::borrow::Cow;

  use super::super::tokens::Token;
  use super::*;
  use pretty_assertions::assert_eq;

  #[test]
  fn it_tokenizes_string() {
    assert_has_tokens(
      r#""t\"est", "\t\r\n\n\u0020 test\n other","#,
      vec![
        Token::String(Cow::Borrowed(r#"t"est"#)),
        Token::Comma,
        Token::String(Cow::Borrowed("\t\r\n\n  test\n other")),
        Token::Comma,
      ],
    );
  }

  #[test]
  fn it_errors_escaping_single_quote_in_double_quote() {
    assert_has_error(
      r#""t\'est""#,
      "Invalid escape in double quote string on line 1 column 3",
    );
  }

  #[test]
  fn it_tokenizes_single_quote_string() {
    assert_has_tokens(
      r#"'t\'est','a',"#,
      vec![
        Token::String(Cow::Borrowed(r#"t'est"#)),
        Token::Comma,
        Token::String(Cow::Borrowed("a")),
        Token::Comma,
      ],
    );
  }

  #[test]
  fn it_errors_escaping_double_quote_in_single_quote() {
    assert_has_error(
      r#"'t\"est'"#,
      "Invalid escape in single quote string on line 1 column 3",
    );
  }

  #[test]
  fn it_errors_for_word_starting_with_invalid_token() {
    assert_has_error(r#"{ &test }"#, "Unexpected token on line 1 column 3");
  }

  #[test]
  fn it_tokenizes_numbers() {
    assert_has_tokens(
      "0, 0.123, -198, 0e-345, 0.3e+025, 1e1,",
      vec![
        Token::Number("0"),
        Token::Comma,
        Token::Number("0.123"),
        Token::Comma,
        Token::Number("-198"),
        Token::Comma,
        Token::Number("0e-345"),
        Token::Comma,
        Token::Number("0.3e+025"),
        Token::Comma,
        Token::Number("1e1"),
        Token::Comma,
      ],
    );
  }

  #[test]
  fn it_tokenizes_hexadecimal_numbers() {
    assert_has_tokens(
      "0x7DF, 0xFF, 0x123ABC, 0xabc, 0X1F",
      vec![
        Token::Number("0x7DF"),
        Token::Comma,
        Token::Number("0xFF"),
        Token::Comma,
        Token::Number("0x123ABC"),
        Token::Comma,
        Token::Number("0xabc"),
        Token::Comma,
        Token::Number("0X1F"),
      ],
    );
  }

  #[test]
  fn it_tokenizes_unary_plus_numbers() {
    assert_has_tokens(
      "+42, +0.5, +1e10, +0xFF",
      vec![
        Token::Number("+42"),
        Token::Comma,
        Token::Number("+0.5"),
        Token::Comma,
        Token::Number("+1e10"),
        Token::Comma,
        Token::Number("+0xFF"),
      ],
    );
  }

  #[test]
  fn it_errors_invalid_exponent() {
    assert_has_error(
      r#"1ea"#,
      "Expected plus, minus, or digit in number literal on line 1 column 3",
    );
    assert_has_error(r#"1e-a"#, "Expected digit on line 1 column 4");
  }

  #[test]
  fn it_tokenizes_simple_tokens() {
    assert_has_tokens(
      "{}[],:true,false,null,",
      vec![
        Token::OpenBrace,
        Token::CloseBrace,
        Token::OpenBracket,
        Token::CloseBracket,
        Token::Comma,
        Token::Colon,
        Token::Boolean(true),
        Token::Comma,
        Token::Boolean(false),
        Token::Comma,
        Token::Null,
        Token::Comma,
      ],
    );
  }

  #[test]
  fn it_tokenizes_comment_line() {
    assert_has_tokens(
      "//test\n//t\r\n// test\n,",
      vec![
        Token::CommentLine("test"),
        Token::CommentLine("t"),
        Token::CommentLine(" test"),
        Token::Comma,
      ],
    );
  }

  #[test]
  fn it_tokenizes_comment_blocks() {
    assert_has_tokens(
      "/*test\n *//* test*/,",
      vec![
        Token::CommentBlock("test\n "),
        Token::CommentBlock(" test"),
        Token::Comma,
      ],
    );
  }

  #[test]
  fn it_errors_on_invalid_utf8_char_for_issue_6() {
    assert_has_error(
      "\"\\uDF06\"",
      "Invalid unicode escape sequence. 'DF06 (unpaired low surrogate)' is not a valid UTF8 character on line 1 column 2",
    );
  }

  fn assert_has_tokens(text: &str, tokens: Vec<Token>) {
    let mut scanner = Scanner::new(text, &Default::default());
    let mut scanned_tokens = Vec::new();

    loop {
      match scanner.scan() {
        Ok(Some(token)) => scanned_tokens.push(token),
        Ok(None) => break,
        Err(err) => panic!("Error parsing: {:?}", err),
      }
    }

    assert_eq!(scanned_tokens, tokens);
  }

  fn assert_has_error(text: &str, message: &str) {
    let mut scanner = Scanner::new(text, &Default::default());
    let mut error_message = String::new();

    loop {
      match scanner.scan() {
        Ok(Some(_)) => {}
        Ok(None) => break,
        Err(err) => {
          error_message = err.to_string();
          break;
        }
      }
    }

    assert_eq!(error_message, message);
  }
}
