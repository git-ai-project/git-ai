use super::ParseOptions;
use super::errors::*;
use super::tokens::Token;
use super::value::*;
use crate::parser::JsoncParser;
use crate::value::Map;

/// Parses a string containing JSONC to a `JsonValue`.
///
/// Returns `None` when the provided string is empty or whitespace.
///
/// # Example
///
/// ```
/// use jsonc_parser::parse_to_value;
///
/// let json_value = parse_to_value(r#"{ "test": 5 } // test"#, &Default::default()).expect("Should parse.");
/// ```
pub fn parse_to_value<'a>(text: &'a str, options: &ParseOptions) -> Result<Option<JsonValue<'a>>, ParseError> {
  let mut parser = JsoncParser::new(text, options);

  let token = parser.scan()?;
  let value = match token {
    Some(token) => parse_value(&mut parser, token)?,
    None => return Ok(None),
  };

  if parser.scan()?.is_some() {
    return Err(
      parser
        .scanner
        .create_error_for_current_token(ParseErrorKind::MultipleRootJsonValues),
    );
  }

  Ok(Some(value))
}

fn parse_value<'a>(parser: &mut JsoncParser<'a>, token: Token<'a>) -> Result<JsonValue<'a>, ParseError> {
  match token {
    Token::OpenBrace => parse_object(parser),
    Token::OpenBracket => parse_array(parser),
    Token::String(s) => Ok(JsonValue::String(s)),
    Token::Number(n) => Ok(JsonValue::Number(n)),
    Token::Boolean(b) => Ok(JsonValue::Boolean(b)),
    Token::Null => Ok(JsonValue::Null),
    other => Err(parser.unexpected_token_error(&other)),
  }
}

fn parse_object<'a>(parser: &mut JsoncParser<'a>) -> Result<JsonValue<'a>, ParseError> {
  parser.enter_container()?;
  let mut props = Map::new();
  let mut first = true;

  loop {
    match parser.scan_object_entry(first)? {
      None => break,
      Some(key) => {
        first = false;
        let key_string = key.into_string();
        parser.scan_object_colon()?;
        match parser.scan()? {
          Some(value_token) => {
            let value = parse_value(parser, value_token)?;
            props.insert(key_string, value);
          }
          None => {
            parser.exit_container();
            return Err(
              parser
                .scanner
                .create_error_for_current_token(ParseErrorKind::ExpectedObjectValue),
            );
          }
        }
      }
    }
  }

  parser.exit_container();
  Ok(JsonValue::Object(JsonObject::new(props)))
}

fn parse_array<'a>(parser: &mut JsoncParser<'a>) -> Result<JsonValue<'a>, ParseError> {
  parser.enter_container()?;
  let mut elements = Vec::new();

  let mut token = parser.scan()?;

  loop {
    match token {
      Some(Token::CloseBracket) => break,
      None => {
        parser.exit_container();
        return Err(
          parser
            .scanner
            .create_error_for_current_token(ParseErrorKind::UnterminatedArray),
        );
      }
      Some(value_token) => {
        elements.push(parse_value(parser, value_token)?);
      }
    }
    token = parser.scan_array_comma()?;
  }

  parser.exit_container();
  Ok(JsonValue::Array(JsonArray::new(elements)))
}

#[cfg(test)]
mod tests {
  use crate::errors::ParseErrorKind;

  use super::*;
  use std::borrow::Cow;

  #[test]
  fn it_should_parse_object() {
    let value = parse_to_value(
      r#"{
    "a": null,
    "b": [null, "text"],
    "c": true,
    d: 25.55
}"#,
      &Default::default(),
    )
    .unwrap()
    .unwrap();

    let mut object_map = Map::new();
    object_map.insert(String::from("a"), JsonValue::Null);
    object_map.insert(
      String::from("b"),
      JsonValue::Array(vec![JsonValue::Null, JsonValue::String(Cow::Borrowed("text"))].into()),
    );
    object_map.insert(String::from("c"), JsonValue::Boolean(true));
    object_map.insert(String::from("d"), JsonValue::Number("25.55"));
    assert_eq!(value, JsonValue::Object(object_map.into()));
  }

  #[test]
  fn it_should_parse_boolean_false() {
    let value = parse_to_value("false", &Default::default()).unwrap().unwrap();
    assert_eq!(value, JsonValue::Boolean(false));
    let value = parse_to_value("true", &Default::default()).unwrap().unwrap();
    assert_eq!(value, JsonValue::Boolean(true));
  }

  #[test]
  fn it_should_parse_boolean_true() {
    let value = parse_to_value("true", &Default::default()).unwrap().unwrap();
    assert_eq!(value, JsonValue::Boolean(true));
  }

  #[test]
  fn it_should_parse_number() {
    let value = parse_to_value("50", &Default::default()).unwrap().unwrap();
    assert_eq!(value, JsonValue::Number("50"));
  }

  #[test]
  fn it_should_parse_string() {
    let value = parse_to_value(r#""test""#, &Default::default()).unwrap().unwrap();
    assert_eq!(value, JsonValue::String(Cow::Borrowed("test")));
  }

  #[test]
  fn it_should_parse_string_with_quotes() {
    let value = parse_to_value(r#""echo \"test\"""#, &Default::default())
      .unwrap()
      .unwrap();
    assert_eq!(value, JsonValue::String(Cow::Borrowed(r#"echo "test""#)));
  }

  #[test]
  fn it_should_parse_array() {
    let value = parse_to_value(r#"[false, true]"#, &Default::default())
      .unwrap()
      .unwrap();
    assert_eq!(
      value,
      JsonValue::Array(vec![JsonValue::Boolean(false), JsonValue::Boolean(true)].into())
    );
  }

  #[test]
  fn it_should_parse_null() {
    let value = parse_to_value("null", &Default::default()).unwrap().unwrap();
    assert_eq!(value, JsonValue::Null);
  }

  #[test]
  fn it_should_parse_empty() {
    let value = parse_to_value("", &Default::default()).unwrap();
    assert!(value.is_none());
  }

  #[test]
  fn error_unexpected_token() {
    let err = parse_to_value("{\n  \"a\":\u{200b}5 }", &Default::default())
      .err()
      .unwrap();
    assert_eq!(err.range().start, 8);
    assert_eq!(err.range().end, 11);
    assert!(matches!(err.kind(), ParseErrorKind::UnexpectedToken));
  }

  #[test]
  fn error_unexpected_word_should_have_full_range() {
    // https://github.com/dprint/jsonc-parser/issues/2
    // the error range should cover the entire word, not just the first character
    let err = parse_to_value(r#"{ "test": asdf }"#, &Default::default())
      .err()
      .unwrap();
    assert!(matches!(err.kind(), ParseErrorKind::UnexpectedWord));
    // "asdf" is at bytes 10..14
    assert_eq!(err.range().start, 10);
    assert_eq!(err.range().end, 14);
  }

  #[test]
  fn it_should_parse_surrogate_pair() {
    // RFC 8259 § 7: non-BMP character 𝄞 (U+1D11E) should be escaped as surrogate pair \uD834\uDD1E
    let src = r#""\uD834\uDD1E""#;
    let v = parse_to_value(src, &Default::default()).unwrap().unwrap();
    if let JsonValue::String(s) = v {
      assert_eq!("\u{1D11E}", s.as_ref());
    } else {
      panic!("Expected string value, got {:?}", v);
    }
  }

  #[test]
  fn it_should_parse_multiple_surrogate_pairs() {
    let src = r#""\uD834\uDD1E\uD834\uDD1E""#;
    let v = parse_to_value(src, &Default::default()).unwrap().unwrap();
    if let JsonValue::String(s) = v {
      assert_eq!("\u{1D11E}\u{1D11E}", s.as_ref());
    } else {
      panic!("Expected string value, got {:?}", v);
    }
  }

  #[test]
  fn it_should_parse_mixed_escapes_with_surrogate_pairs() {
    // "A𝄞B" where 𝄞 is encoded as surrogate pair
    let src = r#""\u0041\uD834\uDD1E\u0042""#;
    let v = parse_to_value(src, &Default::default()).unwrap().unwrap();
    if let JsonValue::String(s) = v {
      assert_eq!("A\u{1D11E}B", s.as_ref());
    } else {
      panic!("Expected string value, got {:?}", v);
    }
  }

  #[test]
  fn it_should_error_on_unpaired_high_surrogate_with_text() {
    let src = r#""\uD834x""#;
    let err = parse_to_value(src, &Default::default()).err().unwrap();
    assert!(err.to_string().contains("unpaired high surrogate"));
  }

  #[test]
  fn it_should_error_on_unpaired_high_surrogate_at_eof() {
    let src = r#""\uD834""#;
    let err = parse_to_value(src, &Default::default()).err().unwrap();
    assert!(err.to_string().contains("unpaired high surrogate"));
  }

  #[test]
  fn it_should_error_on_high_surrogate_followed_by_non_low_surrogate() {
    let src = r#""\uD834\u0041""#;
    let err = parse_to_value(src, &Default::default()).err().unwrap();
    assert!(err.to_string().contains("not followed by low surrogate"));
  }

  #[test]
  fn it_should_error_on_unpaired_low_surrogate() {
    // This test verifies existing behavior is maintained
    let src = r#""\uDC00""#;
    let err = parse_to_value(src, &Default::default()).err().unwrap();
    assert!(err.to_string().contains("unpaired low surrogate"));
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

    let result = parse_to_value(&json, &ParseOptions::default());

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

    let result = parse_to_value(&json, &ParseOptions::default());

    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), "Maximum nesting depth exceeded on line 1 column 2561"),
    }
  }

  // error cases

  #[track_caller]
  fn assert_has_error(text: &str, message: &str) {
    let result = parse_to_value(text, &Default::default());
    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), message),
    }
  }

  #[track_caller]
  fn assert_has_strict_error(text: &str, message: &str) {
    let result = parse_to_value(
      text,
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
  fn it_should_error_when_has_multiple_values() {
    assert_has_error(
      "[][]",
      "Text cannot contain more than one JSON value on line 1 column 3",
    );
  }

  #[test]
  fn it_should_error_when_object_is_not_terminated() {
    assert_has_error("{", "Unterminated object on line 1 column 2");
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
    assert_has_error("[", "Unterminated array on line 1 column 2");
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

  #[test]
  fn missing_comma_between_properties() {
    let text = r#"{
  "name": "alice"
  "age": 25
}"#;
    let value = parse_to_value(text, &Default::default()).unwrap().unwrap();
    let obj = match &value {
      JsonValue::Object(o) => o,
      _ => panic!("Expected object"),
    };
    assert_eq!(obj.get_number("age").unwrap(), "25");

    // but is strict when strict
    assert_has_strict_error(text, "Expected comma on line 2 column 18");
  }

  #[test]
  fn missing_comma_with_comment_between_properties() {
    // when comments are allowed but missing commas are not,
    // should still detect the missing comma after the comment is skipped
    let result = parse_to_value(
      r#"{
  "name": "alice" // comment here
  "age": 25
}"#,
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
  fn it_should_parse_unquoted_keys_with_hex_and_trailing_comma() {
    let text = r#"{
      CP_CanFuncReqId: 0x7DF,  // 2015
  }"#;
    let value = parse_to_value(text, &Default::default()).unwrap().unwrap();
    let obj = match &value {
      JsonValue::Object(o) => o,
      _ => panic!("Expected object"),
    };
    assert_eq!(obj.get_number("CP_CanFuncReqId").unwrap(), "0x7DF");
  }

  #[test]
  fn it_should_parse_unary_plus_numbers() {
    let value = parse_to_value(r#"{ "test": +42 }"#, &Default::default())
      .unwrap()
      .unwrap();
    let obj = match &value {
      JsonValue::Object(o) => o,
      _ => panic!("Expected object"),
    };
    assert_eq!(obj.get_number("test").unwrap(), "+42");
  }

  #[test]
  fn it_should_parse_large_shallow_objects() {
    // makes sure that nesting depth limit does not affect shallow objects
    let mut json = "{\"q\":[".to_string();
    let size = 1_000;

    for _ in 0..size {
      json += "{\"q\":[{}]}, [\"hello\"], ";
    }

    json += "]}";

    let result = parse_to_value(&json, &ParseOptions::default());
    assert!(result.is_ok());
  }
}
