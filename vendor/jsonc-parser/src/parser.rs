use std::borrow::Cow;

use crate::ParseOptions;
use crate::common::Range;
use crate::errors::*;
use crate::scanner::Scanner;
use crate::scanner::ScannerOptions;
use crate::tokens::Token;

pub(crate) enum ObjectKey<'a> {
  String(Cow<'a, str>),
  Word(&'a str),
}

impl<'a> ObjectKey<'a> {
  pub fn into_string(self) -> String {
    match self {
      ObjectKey::String(s) => s.into_owned(),
      ObjectKey::Word(s) => s.to_string(),
    }
  }
}

/// Shared JSONC parser infrastructure used by both `parse_to_value` and
/// the serde deserializer. Handles scanning, comment skipping, depth
/// tracking, and comma/separator logic.
pub(crate) struct JsoncParser<'a> {
  pub scanner: Scanner<'a>,
  #[allow(dead_code)] // used by the serde feature
  pub text: &'a str,
  allow_comments: bool,
  allow_trailing_commas: bool,
  allow_missing_commas: bool,
  allow_loose_object_property_names: bool,
  depth: usize,
  pending_token: Option<Token<'a>>,
}

impl<'a> JsoncParser<'a> {
  pub fn new(text: &'a str, options: &ParseOptions) -> Self {
    Self {
      scanner: Scanner::new(
        text,
        &ScannerOptions {
          allow_single_quoted_strings: options.allow_single_quoted_strings,
          allow_hexadecimal_numbers: options.allow_hexadecimal_numbers,
          allow_unary_plus_numbers: options.allow_unary_plus_numbers,
        },
      ),
      text,
      allow_comments: options.allow_comments,
      allow_trailing_commas: options.allow_trailing_commas,
      allow_missing_commas: options.allow_missing_commas,
      allow_loose_object_property_names: options.allow_loose_object_property_names,
      depth: 0,
      pending_token: None,
    }
  }

  /// Scans the next non-comment token. Returns a pending token if one
  /// was put back via `put_back`.
  pub fn scan(&mut self) -> Result<Option<Token<'a>>, ParseError> {
    if let Some(token) = self.pending_token.take() {
      return Ok(Some(token));
    }
    loop {
      match self.scanner.scan()? {
        Some(Token::CommentLine(_) | Token::CommentBlock(_)) => {
          if !self.allow_comments {
            return Err(
              self
                .scanner
                .create_error_for_current_token(ParseErrorKind::CommentsNotAllowed),
            );
          }
          continue;
        }
        token => return Ok(token),
      }
    }
  }

  /// Puts a token back so the next `scan()` returns it.
  #[cfg(feature = "serde")]
  pub fn put_back(&mut self, token: Token<'a>) {
    debug_assert!(self.pending_token.is_none(), "put_back called with pending token");
    self.pending_token = Some(token);
  }

  /// Increments depth and checks the nesting limit.
  pub fn enter_container(&mut self) -> Result<(), ParseError> {
    self.depth += 1;
    if self.depth > 512 {
      self.depth -= 1;
      Err(
        self
          .scanner
          .create_error_for_current_token(ParseErrorKind::NestingDepthExceeded),
      )
    } else {
      Ok(())
    }
  }

  /// Decrements depth.
  pub fn exit_container(&mut self) {
    self.depth -= 1;
  }

  /// Returns an error appropriate for an unexpected token.
  pub fn unexpected_token_error(&self, token: &Token) -> ParseError {
    let kind = match token {
      Token::CloseBracket => ParseErrorKind::UnexpectedCloseBracket,
      Token::CloseBrace => ParseErrorKind::UnexpectedCloseBrace,
      Token::Comma => ParseErrorKind::UnexpectedComma,
      Token::Colon => ParseErrorKind::UnexpectedColon,
      Token::Word(_) => ParseErrorKind::UnexpectedWord,
      _ => ParseErrorKind::UnexpectedToken,
    };
    self.scanner.create_error_for_current_token(kind)
  }

  /// Scans the next object entry (key or close brace), handling commas
  /// between entries. Pass `first = true` for the first entry.
  pub fn scan_object_entry(&mut self, first: bool) -> Result<Option<ObjectKey<'a>>, ParseError> {
    if first {
      return self.scan_object_key();
    }

    let after_value_end = self.scanner.token_end();
    let token = self.scan()?;
    match token {
      Some(Token::Comma) => {
        let comma_range = Range::new(self.scanner.token_start(), self.scanner.token_end());
        let key = self.scan_object_key()?;
        if key.is_none() && !self.allow_trailing_commas {
          return Err(
            self
              .scanner
              .create_error_for_range(comma_range, ParseErrorKind::TrailingCommasNotAllowed),
          );
        }
        Ok(key)
      }
      Some(Token::CloseBrace) => Ok(None),
      Some(Token::String(s)) if self.allow_missing_commas => Ok(Some(ObjectKey::String(s))),
      Some(Token::Word(s) | Token::Number(s)) if self.allow_missing_commas => {
        if !self.allow_loose_object_property_names {
          return Err(
            self
              .scanner
              .create_error_for_current_token(ParseErrorKind::ExpectedStringObjectProperty),
          );
        }
        Ok(Some(ObjectKey::Word(s)))
      }
      Some(Token::String(_) | Token::Word(_) | Token::Number(_)) => {
        let range = Range::new(after_value_end, after_value_end);
        Err(
          self
            .scanner
            .create_error_for_range(range, ParseErrorKind::ExpectedComma),
        )
      }
      None => Err(
        self
          .scanner
          .create_error_for_current_token(ParseErrorKind::UnterminatedObject),
      ),
      _ => Err(
        self
          .scanner
          .create_error_for_current_token(ParseErrorKind::UnexpectedTokenInObject),
      ),
    }
  }

  /// Scans an object property colon separator.
  pub fn scan_object_colon(&mut self) -> Result<(), ParseError> {
    match self.scan()? {
      Some(Token::Colon) => Ok(()),
      _ => Err(
        self
          .scanner
          .create_error_for_current_token(ParseErrorKind::ExpectedColonAfterObjectKey),
      ),
    }
  }

  /// After an array element, scans for the comma/close-bracket and
  /// returns the next token.
  pub fn scan_array_comma(&mut self) -> Result<Option<Token<'a>>, ParseError> {
    let token = self.scan()?;
    if matches!(&token, Some(Token::Comma)) {
      let comma_range = Range::new(self.scanner.token_start(), self.scanner.token_end());
      let next = self.scan()?;
      if matches!(&next, Some(Token::CloseBracket)) && !self.allow_trailing_commas {
        return Err(
          self
            .scanner
            .create_error_for_range(comma_range, ParseErrorKind::TrailingCommasNotAllowed),
        );
      }
      Ok(next)
    } else {
      Ok(token)
    }
  }

  fn scan_object_key(&mut self) -> Result<Option<ObjectKey<'a>>, ParseError> {
    match self.scan()? {
      Some(Token::CloseBrace) => Ok(None),
      Some(Token::String(s)) => Ok(Some(ObjectKey::String(s))),
      Some(Token::Word(s) | Token::Number(s)) => {
        if !self.allow_loose_object_property_names {
          return Err(
            self
              .scanner
              .create_error_for_current_token(ParseErrorKind::ExpectedStringObjectProperty),
          );
        }
        Ok(Some(ObjectKey::Word(s)))
      }
      None => Err(
        self
          .scanner
          .create_error_for_current_token(ParseErrorKind::UnterminatedObject),
      ),
      _ => Err(
        self
          .scanner
          .create_error_for_current_token(ParseErrorKind::UnexpectedTokenInObject),
      ),
    }
  }
}
