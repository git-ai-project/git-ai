use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::common::Range;
use super::errors::*;
use super::scanner::Scanner;
use super::scanner::ScannerOptions;
use super::tokens::Token;
use super::tokens::TokenAndRange;

/// Map where the comments are stored in collections where
/// the key is the previous token end or start of file or
/// next token start or end of the file.
pub type CommentMap<'a> = HashMap<usize, Rc<Vec<Comment<'a>>>>;

/// Strategy for handling comments during parsing.
///
/// This enum determines how comments in the JSON/JSONC input are collected
/// and represented in the resulting abstract syntax tree (AST).
#[derive(Default, Debug, PartialEq, Clone)]
pub enum CommentCollectionStrategy {
  /// Comments are not collected and are effectively ignored during parsing.
  #[default]
  Off,
  /// Comments are collected and stored separately from the main AST structure.
  ///
  /// When this strategy is used, comments are placed in a [`CommentMap`] where
  /// the key is the previous token end or start of file, or the next token start
  /// or end of file.
  Separate,
  /// Comments are collected and treated as tokens within the AST.
  ///
  /// When this strategy is used, comments appear alongside other tokens in the
  /// token stream when `tokens: true` is set in [`CollectOptions`].
  AsTokens,
}

/// Options for collecting comments and tokens.
#[derive(Default, Clone)]
pub struct CollectOptions {
  /// Include comments in the result.
  pub comments: CommentCollectionStrategy,
  /// Include tokens in the result.
  pub tokens: bool,
}

/// Options for parsing.
#[derive(Clone)]
pub struct ParseOptions {
  /// Allow comments (defaults to `true`).
  pub allow_comments: bool,
  /// Allow words and numbers as object property names (defaults to `true`).
  pub allow_loose_object_property_names: bool,
  /// Allow trailing commas on object literal and array literal values (defaults to `true`).
  pub allow_trailing_commas: bool,
  /// Allow missing commas between object properties or array elements (defaults to `true`).
  pub allow_missing_commas: bool,
  /// Allow single-quoted strings (defaults to `true`).
  pub allow_single_quoted_strings: bool,
  /// Allow hexadecimal numbers like 0xFF (defaults to `true`).
  pub allow_hexadecimal_numbers: bool,
  /// Allow unary plus sign on numbers like +42 (defaults to `true`).
  pub allow_unary_plus_numbers: bool,
}

impl Default for ParseOptions {
  fn default() -> Self {
    Self {
      allow_comments: true,
      allow_loose_object_property_names: true,
      allow_trailing_commas: true,
      allow_missing_commas: true,
      allow_single_quoted_strings: true,
      allow_hexadecimal_numbers: true,
      allow_unary_plus_numbers: true,
    }
  }
}

/// Result of parsing the text.
pub struct ParseResult<'a> {
  /// Collection of comments in the text.
  ///
  /// Provide `comments: true` to the `ParseOptions` for this to have a value.
  ///
  /// Remarks: The key is the start and end position of the tokens.
  pub comments: Option<CommentMap<'a>>,
  /// The JSON value the text contained.
  pub value: Option<Value<'a>>,
  /// Collection of tokens (excluding any comments).
  ///
  /// Provide `tokens: true` to the `ParseOptions` for this to have a value.
  pub tokens: Option<Vec<TokenAndRange<'a>>>,
}

struct Context<'a> {
  scanner: Scanner<'a>,
  comments: Option<CommentMap<'a>>,
  current_comments: Option<Vec<Comment<'a>>>,
  last_token_end: usize,
  range_stack: Vec<Range>,
  tokens: Option<Vec<TokenAndRange<'a>>>,
  collect_comments_as_tokens: bool,
  allow_comments: bool,
  allow_trailing_commas: bool,
  allow_missing_commas: bool,
  allow_loose_object_property_names: bool,
  maximum_nesting_depth: usize,
}

impl<'a> Context<'a> {
  pub fn scan(&mut self) -> Result<Option<Token<'a>>, ParseError> {
    let previous_end = self.last_token_end;
    let token = self.scan_handling_comments()?;
    self.last_token_end = self.scanner.token_end();

    // store the comment for the previous token end, and current token start
    if let Some(comments) = self.comments.as_mut()
      && let Some(current_comments) = self.current_comments.take()
    {
      let current_comments = Rc::new(current_comments);
      comments.insert(previous_end, current_comments.clone());
      comments.insert(self.scanner.token_start(), current_comments);
    }

    if let Some(token) = &token
      && self.tokens.is_some()
    {
      self.capture_token(token.clone());
    }

    Ok(token)
  }

  pub fn token(&self) -> Option<Token<'a>> {
    self.scanner.token()
  }

  pub fn start_range(&mut self) {
    self.range_stack.push(Range {
      start: self.scanner.token_start(),
      end: 0,
    });
  }

  pub fn end_range(&mut self) -> Range {
    let mut range = self
      .range_stack
      .pop()
      .expect("Range was popped from the stack, but the stack was empty.");
    range.end = self.scanner.token_end();
    range
  }

  pub fn create_range_from_last_token(&self) -> Range {
    Range {
      start: self.scanner.token_start(),
      end: self.scanner.token_end(),
    }
  }

  pub fn create_error(&self, kind: ParseErrorKind) -> ParseError {
    self.scanner.create_error_for_current_token(kind)
  }

  pub fn create_error_for_current_range(&mut self, kind: ParseErrorKind) -> ParseError {
    let range = self.end_range();
    self.create_error_for_range(range, kind)
  }

  pub fn create_error_for_range(&self, range: Range, kind: ParseErrorKind) -> ParseError {
    self.scanner.create_error_for_range(range, kind)
  }

  fn scan_handling_comments(&mut self) -> Result<Option<Token<'a>>, ParseError> {
    loop {
      let token = self.scanner.scan()?;
      match token {
        Some(token @ Token::CommentLine(_) | token @ Token::CommentBlock(_)) if self.collect_comments_as_tokens => {
          self.capture_token(token);
        }
        Some(Token::CommentLine(text)) => {
          self.handle_comment(Comment::Line(CommentLine {
            range: self.create_range_from_last_token(),
            text,
          }))?;
        }
        Some(Token::CommentBlock(text)) => {
          self.handle_comment(Comment::Block(CommentBlock {
            range: self.create_range_from_last_token(),
            text,
          }))?;
        }
        _ => return Ok(token),
      }
    }
  }

  fn capture_token(&mut self, token: Token<'a>) {
    let range = self.create_range_from_last_token();
    if let Some(tokens) = self.tokens.as_mut() {
      tokens.push(TokenAndRange {
        token: token.clone(),
        range,
      });
    }
  }

  fn handle_comment(&mut self, comment: Comment<'a>) -> Result<(), ParseError> {
    if !self.allow_comments {
      return Err(self.create_error(ParseErrorKind::CommentsNotAllowed));
    }

    if self.comments.is_some() {
      if let Some(comments) = self.current_comments.as_mut() {
        comments.push(comment);
      } else {
        self.current_comments = Some(vec![comment]);
      }
    }

    Ok(())
  }
}

/// Parses a string containing JSONC to an AST with comments and tokens.
///
/// # Example
///
/// ```
/// use jsonc_parser::CollectOptions;
/// use jsonc_parser::CommentCollectionStrategy;
/// use jsonc_parser::parse_to_ast;
/// use jsonc_parser::ParseOptions;
///
/// let parse_result = parse_to_ast(r#"{ "test": 5 } // test"#, &CollectOptions {
///     comments: CommentCollectionStrategy::Separate, // include comments in result
///     tokens: true, // include tokens in result
/// }, &Default::default()).expect("Should parse.");
/// // ...inspect parse_result for value, tokens, and comments here...
/// ```
pub fn parse_to_ast<'a>(
  text: &'a str,
  collect_options: &CollectOptions,
  parse_options: &ParseOptions,
) -> Result<ParseResult<'a>, ParseError> {
  let mut context = Context {
    scanner: Scanner::new(
      text,
      &ScannerOptions {
        allow_single_quoted_strings: parse_options.allow_single_quoted_strings,
        allow_hexadecimal_numbers: parse_options.allow_hexadecimal_numbers,
        allow_unary_plus_numbers: parse_options.allow_unary_plus_numbers,
      },
    ),
    comments: match collect_options.comments {
      CommentCollectionStrategy::Separate => Some(Default::default()),
      CommentCollectionStrategy::Off | CommentCollectionStrategy::AsTokens => None,
    },
    current_comments: None,
    last_token_end: 0,
    range_stack: Vec::new(),
    tokens: if collect_options.tokens { Some(Vec::new()) } else { None },
    collect_comments_as_tokens: collect_options.comments == CommentCollectionStrategy::AsTokens,
    allow_comments: parse_options.allow_comments,
    allow_trailing_commas: parse_options.allow_trailing_commas,
    allow_missing_commas: parse_options.allow_missing_commas,
    allow_loose_object_property_names: parse_options.allow_loose_object_property_names,
    maximum_nesting_depth: 512,
  };
  context.scan()?;
  let value = parse_value(&mut context)?;

  if context.scan()?.is_some() {
    return Err(context.create_error(ParseErrorKind::MultipleRootJsonValues));
  }

  debug_assert!(context.range_stack.is_empty());

  Ok(ParseResult {
    comments: context.comments,
    tokens: context.tokens,
    value,
  })
}

fn parse_value<'a>(context: &mut Context<'a>) -> Result<Option<Value<'a>>, ParseError> {
  if context.range_stack.len() > context.maximum_nesting_depth {
    return Err(context.create_error_for_current_range(ParseErrorKind::NestingDepthExceeded));
  }

  match context.token() {
    None => Ok(None),
    Some(token) => match token {
      Token::OpenBrace => Ok(Some(Value::Object(parse_object(context)?))),
      Token::OpenBracket => Ok(Some(Value::Array(parse_array(context)?))),
      Token::String(value) => Ok(Some(Value::StringLit(create_string_lit(context, value)))),
      Token::Boolean(value) => Ok(Some(Value::BooleanLit(create_boolean_lit(context, value)))),
      Token::Number(value) => Ok(Some(Value::NumberLit(create_number_lit(context, value)))),
      Token::Null => Ok(Some(Value::NullKeyword(create_null_keyword(context)))),
      Token::CloseBracket => Err(context.create_error(ParseErrorKind::UnexpectedCloseBracket)),
      Token::CloseBrace => Err(context.create_error(ParseErrorKind::UnexpectedCloseBrace)),
      Token::Comma => Err(context.create_error(ParseErrorKind::UnexpectedComma)),
      Token::Colon => Err(context.create_error(ParseErrorKind::UnexpectedColon)),
      Token::Word(_) => Err(context.create_error(ParseErrorKind::UnexpectedWord)),
      Token::CommentLine(_) => unreachable!(),
      Token::CommentBlock(_) => unreachable!(),
    },
  }
}

fn parse_object<'a>(context: &mut Context<'a>) -> Result<Object<'a>, ParseError> {
  debug_assert!(context.token() == Some(Token::OpenBrace));
  let mut properties = Vec::new();

  context.start_range();
  context.scan()?;

  loop {
    match context.token() {
      Some(Token::CloseBrace) => break,
      Some(Token::String(prop_name)) => {
        properties.push(parse_object_property(context, PropName::String(prop_name))?);
      }
      Some(Token::Word(prop_name)) | Some(Token::Number(prop_name)) => {
        properties.push(parse_object_property(context, PropName::Word(prop_name))?);
      }
      None => return Err(context.create_error_for_current_range(ParseErrorKind::UnterminatedObject)),
      _ => return Err(context.create_error(ParseErrorKind::UnexpectedTokenInObject)),
    }

    // skip the comma
    let after_value_end = context.last_token_end;
    match context.scan()? {
      Some(Token::Comma) => {
        let comma_range = context.create_range_from_last_token();
        if let Some(Token::CloseBrace) = context.scan()?
          && !context.allow_trailing_commas
        {
          return Err(context.create_error_for_range(comma_range, ParseErrorKind::TrailingCommasNotAllowed));
        }
      }
      Some(Token::String(_) | Token::Word(_) | Token::Number(_)) if !context.allow_missing_commas => {
        let range = Range {
          start: after_value_end,
          end: after_value_end,
        };
        return Err(context.create_error_for_range(range, ParseErrorKind::ExpectedComma));
      }
      _ => {}
    }
  }

  Ok(Object {
    range: context.end_range(),
    properties,
  })
}

enum PropName<'a> {
  String(Cow<'a, str>),
  Word(&'a str),
}

fn parse_object_property<'a>(context: &mut Context<'a>, prop_name: PropName<'a>) -> Result<ObjectProp<'a>, ParseError> {
  context.start_range();

  let name = match prop_name {
    PropName::String(prop_name) => ObjectPropName::String(create_string_lit(context, prop_name)),
    PropName::Word(prop_name) => {
      if context.allow_loose_object_property_names {
        ObjectPropName::Word(create_word(context, prop_name))
      } else {
        return Err(context.create_error(ParseErrorKind::ExpectedStringObjectProperty));
      }
    }
  };

  match context.scan()? {
    Some(Token::Colon) => {}
    _ => return Err(context.create_error(ParseErrorKind::ExpectedColonAfterObjectKey)),
  }

  context.scan()?;
  let value = parse_value(context)?;

  match value {
    Some(value) => Ok(ObjectProp {
      range: context.end_range(),
      name,
      value,
    }),
    None => Err(context.create_error(ParseErrorKind::ExpectedObjectValue)),
  }
}

fn parse_array<'a>(context: &mut Context<'a>) -> Result<Array<'a>, ParseError> {
  debug_assert!(context.token() == Some(Token::OpenBracket));
  let mut elements = Vec::new();

  context.start_range();
  context.scan()?;

  loop {
    match context.token() {
      Some(Token::CloseBracket) => break,
      None => return Err(context.create_error_for_current_range(ParseErrorKind::UnterminatedArray)),
      _ => match parse_value(context)? {
        Some(value) => elements.push(value),
        None => return Err(context.create_error_for_current_range(ParseErrorKind::UnterminatedArray)),
      },
    }

    // skip the comma
    if let Some(Token::Comma) = context.scan()? {
      let comma_range = context.create_range_from_last_token();
      if let Some(Token::CloseBracket) = context.scan()?
        && !context.allow_trailing_commas
      {
        return Err(context.create_error_for_range(comma_range, ParseErrorKind::TrailingCommasNotAllowed));
      }
    }
  }

  Ok(Array {
    range: context.end_range(),
    elements,
  })
}

// factory functions

fn create_string_lit<'a>(context: &Context<'a>, value: Cow<'a, str>) -> StringLit<'a> {
  StringLit {
    range: context.create_range_from_last_token(),
    value,
  }
}

fn create_word<'a>(context: &Context<'a>, value: &'a str) -> WordLit<'a> {
  WordLit {
    range: context.create_range_from_last_token(),
    value,
  }
}

fn create_boolean_lit(context: &Context, value: bool) -> BooleanLit {
  BooleanLit {
    range: context.create_range_from_last_token(),
    value,
  }
}

fn create_number_lit<'a>(context: &Context<'a>, value: &'a str) -> NumberLit<'a> {
  NumberLit {
    range: context.create_range_from_last_token(),
    value,
  }
}

fn create_null_keyword(context: &Context) -> NullKeyword {
  NullKeyword {
    range: context.create_range_from_last_token(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;

  #[test]
  fn it_should_error_when_has_multiple_values() {
    assert_has_error(
      "[][]",
      "Text cannot contain more than one JSON value on line 1 column 3",
    );
  }

  #[test]
  fn it_should_error_when_object_is_not_terminated() {
    assert_has_error("{", "Unterminated object on line 1 column 1");
  }

  #[test]
  fn it_should_error_when_object_has_unexpected_token() {
    assert_has_error("{ [] }", "Unexpected token in object on line 1 column 3");
  }

  #[test]
  fn it_should_error_when_object_has_two_non_string_tokens() {
    assert_has_error(
      "{ asdf asdf: 5 }",
      "Expected colon after the string or word in object property on line 1 column 8",
    );
  }

  #[test]
  fn it_should_error_when_array_is_not_terminated() {
    assert_has_error("[", "Unterminated array on line 1 column 1");
  }

  #[test]
  fn it_should_error_when_array_has_unexpected_token() {
    assert_has_error("[:]", "Unexpected colon on line 1 column 2");
  }

  #[test]
  fn it_should_error_when_comment_block_not_closed() {
    assert_has_error("/* test", "Unterminated comment block on line 1 column 1");
  }

  #[test]
  fn it_should_error_when_string_lit_not_closed() {
    assert_has_error("\" test", "Unterminated string literal on line 1 column 1");
  }

  fn assert_has_error(text: &str, message: &str) {
    let result = parse_to_ast(text, &Default::default(), &Default::default());
    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), message),
    }
  }

  #[test]
  fn strict_should_error_object_trailing_comma() {
    assert_has_strict_error(
      r#"{ "test": 5, }"#,
      "Trailing commas are not allowed on line 1 column 12",
    );
  }

  #[test]
  fn strict_should_error_array_trailing_comma() {
    assert_has_strict_error(r#"[ "test", ]"#, "Trailing commas are not allowed on line 1 column 9");
  }

  #[test]
  fn strict_should_error_comment_line() {
    assert_has_strict_error(r#"[ "test" ] // 1"#, "Comments are not allowed on line 1 column 12");
  }

  #[test]
  fn strict_should_error_comment_block() {
    assert_has_strict_error(r#"[ "test" /* 1 */]"#, "Comments are not allowed on line 1 column 10");
  }

  #[test]
  fn strict_should_error_word_property() {
    assert_has_strict_error(
      r#"{ word: 5 }"#,
      "Expected string for object property on line 1 column 3",
    );
  }

  #[test]
  fn strict_should_error_single_quoted_string() {
    assert_has_strict_error(
      r#"{ "key": 'value' }"#,
      "Single-quoted strings are not allowed on line 1 column 10",
    );
  }

  #[test]
  fn strict_should_error_hexadecimal_number() {
    assert_has_strict_error(
      r#"{ "key": 0xFF }"#,
      "Hexadecimal numbers are not allowed on line 1 column 10",
    );
  }

  #[test]
  fn strict_should_error_unary_plus_number() {
    assert_has_strict_error(
      r#"{ "key": +42 }"#,
      "Unary plus on numbers is not allowed on line 1 column 10",
    );
  }

  #[track_caller]
  fn assert_has_strict_error(text: &str, message: &str) {
    let result = parse_to_ast(
      text,
      &Default::default(),
      &ParseOptions {
        allow_comments: false,
        allow_loose_object_property_names: false,
        allow_trailing_commas: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
      },
    );
    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), message),
    }
  }

  #[test]
  fn it_should_not_include_tokens_by_default() {
    let result = parse_to_ast("{}", &Default::default(), &Default::default()).unwrap();
    assert!(result.tokens.is_none());
  }

  #[test]
  fn it_should_include_tokens_when_specified() {
    let result = parse_to_ast(
      "{}",
      &CollectOptions {
        tokens: true,
        ..Default::default()
      },
      &Default::default(),
    )
    .unwrap();
    let tokens = result.tokens.unwrap();
    assert_eq!(tokens.len(), 2);
  }

  #[test]
  fn it_should_not_include_comments_by_default() {
    let result = parse_to_ast("{}", &Default::default(), &Default::default()).unwrap();
    assert!(result.comments.is_none());
  }

  #[test]
  fn it_should_include_comments_when_specified() {
    let result = parse_to_ast(
      "{} // 2",
      &CollectOptions {
        comments: CommentCollectionStrategy::Separate,
        ..Default::default()
      },
      &Default::default(),
    )
    .unwrap();
    let comments = result.comments.unwrap();
    assert_eq!(comments.len(), 2); // for both positions, but it's the same comment
  }

  #[cfg(not(feature = "error_unicode_width"))]
  #[test]
  fn error_correct_line_column_unicode_width() {
    assert_has_strict_error(r#"["🧑‍🦰", ["#, "Unterminated array on line 1 column 9");
  }

  #[cfg(feature = "error_unicode_width")]
  #[test]
  fn error_correct_line_column_unicode_width() {
    assert_has_strict_error(r#"["🧑‍🦰", ["#, "Unterminated array on line 1 column 10");
  }

  #[test]
  fn it_should_parse_unquoted_keys_with_hex_and_trailing_comma() {
    let text = r#"{
      CP_CanFuncReqId: 0x7DF,  // 2015
  }"#;
    {
      let parse_result = parse_to_ast(text, &Default::default(), &Default::default()).unwrap();

      let value = parse_result.value.unwrap();
      let obj = value.as_object().unwrap();
      assert_eq!(obj.properties.len(), 1);
      assert_eq!(obj.properties[0].name.as_str(), "CP_CanFuncReqId");

      let number_value = obj.properties[0].value.as_number_lit().unwrap();
      assert_eq!(number_value.value, "0x7DF");
    }
    #[cfg(feature = "serde")]
    {
      let value: serde_json::Value = crate::parse_to_serde_value(text, &Default::default()).unwrap();
      // hexadecimal numbers are converted to decimal in serde output
      assert_eq!(
        value,
        serde_json::json!({
          "CP_CanFuncReqId": 2015
        })
      );
    }
  }

  #[test]
  fn it_should_parse_unary_plus_numbers() {
    let result = parse_to_ast(r#"{ "test": +42 }"#, &Default::default(), &Default::default()).unwrap();

    let value = result.value.unwrap();
    let obj = value.as_object().unwrap();
    assert_eq!(obj.properties.len(), 1);
    assert_eq!(obj.properties[0].name.as_str(), "test");

    let number_value = obj.properties[0].value.as_number_lit().unwrap();
    assert_eq!(number_value.value, "+42");
  }

  #[test]
  fn missing_comma_between_properties() {
    let text = r#"{
  "name": "alice"
  "age": 25
}"#;
    let result = parse_to_ast(text, &Default::default(), &Default::default()).unwrap();
    assert_eq!(
      result
        .value
        .unwrap()
        .as_object()
        .unwrap()
        .get_number("age")
        .unwrap()
        .value,
      "25"
    );

    // but is strict when strict
    assert_has_strict_error(text, "Expected comma on line 2 column 18");
  }

  #[test]
  fn missing_comma_with_comment_between_properties() {
    // when comments are allowed but missing commas are not,
    // should still detect the missing comma after the comment is skipped
    let result = parse_to_ast(
      r#"{
  "name": "alice" // comment here
  "age": 25
}"#,
      &Default::default(),
      &ParseOptions {
        allow_comments: true,
        allow_missing_commas: false,
        ..Default::default()
      },
    );
    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), "Expected comma on line 2 column 18"),
    }
  }

  #[test]
  fn it_should_error_when_arrays_are_deeply_nested() {
    // Deeply nested arrays cause a stack overflow when recursion depth is not limited
    let mut json = String::new();
    let depth = 30_000;

    for _ in 0..depth {
      json += "[";
    }

    for _ in 0..depth {
      json += "]";
    }

    let result = parse_to_ast(&json, &Default::default(), &ParseOptions::default());

    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), "Maximum nesting depth exceeded on line 1 column 513"),
    }
  }

  #[test]
  fn it_should_error_when_objects_are_deeply_nested() {
    // Deeply nested objects cause a stack overflow when recursion depth is not limited
    let mut json = String::new();
    let depth = 30_000;

    for _ in 0..depth {
      json += "{\"q\":";
    }

    for _ in 0..depth {
      json += "}";
    }

    let result = parse_to_ast(&json, &Default::default(), &ParseOptions::default());

    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), "Maximum nesting depth exceeded on line 1 column 1282"),
    }
  }

  #[test]
  fn it_should_parse_large_shallow_objects() {
    // Makes sure that nesting depth limit does not affect shallow objects
    let mut json = "{\"q\":[".to_string();
    let size = 1_000;

    for _ in 0..size {
      json += "{\"q\":[{}]}, [\"hello\"], ";
    }

    json += "]}";

    let result = parse_to_ast(&json, &Default::default(), &ParseOptions::default());

    match result {
      Ok(_) => {}
      Err(_) => panic!("Expected Ok, but did not find one."),
    }
  }
}
