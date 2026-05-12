use std::borrow::Cow;

use ::serde::de::DeserializeSeed;
use ::serde::de::EnumAccess;
use ::serde::de::IntoDeserializer;
use ::serde::de::MapAccess;
use ::serde::de::SeqAccess;
use ::serde::de::VariantAccess;
use ::serde::de::Visitor;
use ::serde::forward_to_deserialize_any;

use super::ParseOptions;
use super::common::Range;
use super::errors::ParseError;
use super::errors::ParseErrorKind;
use super::tokens::Token;
use crate::parser::JsoncParser;

/// Parses a string containing JSONC to a `serde_json::Value` or any
/// type that implements `serde::Deserialize`.
///
/// Requires the "serde" cargo feature:
///
/// ```toml
/// jsonc-parser = { version = "...", features = ["serde"] }
/// ```
///
/// # Example
///
/// Parsing to a `serde_json::Value`:
///
/// ```rs
/// use jsonc_parser::parse_to_serde_value;
///
/// let json_value: serde_json::Value = parse_to_serde_value(
///   r#"{ "test": 5 } // test"#,
///   &Default::default(),
/// ).unwrap();
/// ```
///
/// Or to a concrete type:
///
/// ```rs
/// use jsonc_parser::parse_to_serde_value;
///
/// #[derive(serde::Deserialize)]
/// struct Config {
///   test: u32,
/// }
///
/// let config: Config = parse_to_serde_value(
///   r#"{ "test": 5 } // test"#,
///   &Default::default(),
/// ).unwrap();
/// ```
///
/// Empty or whitespace-only input deserializes as `null`, which means
/// `Option<T>` will produce `None` while other types will return a
/// type-mismatch error.
pub fn parse_to_serde_value<T: ::serde::de::DeserializeOwned>(
  text: &str,
  parse_options: &ParseOptions,
) -> Result<T, ParseError> {
  let mut parser = JsoncParser::new(text, parse_options);

  let token = parser.scan()?;
  match token {
    Some(token) => parser.put_back(token),
    None => parser.put_back(Token::Null),
  }

  let value = T::deserialize(&mut parser)?;
  if parser.scan()?.is_some() {
    return Err(
      parser
        .scanner
        .create_error_for_current_token(ParseErrorKind::MultipleRootJsonValues),
    );
  }
  Ok(value)
}

impl ::serde::de::Error for ParseError {
  fn custom<T: std::fmt::Display>(msg: T) -> Self {
    ParseError::custom_err(msg.to_string())
  }
}

impl<'de> ::serde::Deserializer<'de> for &mut JsoncParser<'de> {
  type Error = ParseError;

  fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
    match self.scan()? {
      None => Err(ParseError::custom_err("unexpected end of input".to_string())),
      Some(token) => deserialize_token(self, token, visitor),
    }
  }

  fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
    match self.scan()? {
      Some(Token::Null) => visitor.visit_none(),
      Some(token) => {
        self.put_back(token);
        visitor.visit_some(self)
      }
      None => visitor.visit_none(),
    }
  }

  fn deserialize_enum<V: Visitor<'de>>(
    self,
    _name: &'static str,
    _variants: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    let token = self.scan()?;
    let token_range = Range::new(self.scanner.token_start(), self.scanner.token_end());
    let text = self.text;
    let result = match token {
      Some(Token::String(s)) => {
        let variant: String = s.into_owned();
        visitor.visit_enum(variant.into_deserializer())
      }
      Some(Token::OpenBrace) => {
        // expect exactly one property: { "Variant": data }
        let key = match self.scan()? {
          Some(Token::String(s)) => s.into_owned(),
          _ => {
            return Err(ParseError::new(
              token_range,
              ParseErrorKind::Custom("expected a string key for enum variant".to_string()),
              text,
            ));
          }
        };

        // expect colon
        self.scan_object_colon()?;

        let result = visitor.visit_enum(ObjectEnumAccess {
          parser: self,
          variant: key,
        });
        result.and_then(|v| {
          // expect close brace
          match self.scan()? {
            Some(Token::CloseBrace) => Ok(v),
            _ => Err(
              self
                .scanner
                .create_error_for_current_token(ParseErrorKind::UnterminatedObject),
            ),
          }
        })
      }
      _ => {
        return Err(ParseError::new(
          token_range,
          ParseErrorKind::Custom("expected a string or object for enum".to_string()),
          text,
        ));
      }
    };
    result.map_err(|e| e.with_position(token_range, text))
  }

  fn deserialize_newtype_struct<V: Visitor<'de>>(
    self,
    _name: &'static str,
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    visitor.visit_newtype_struct(self)
  }

  forward_to_deserialize_any! {
    bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64
    char str string bytes byte_buf unit unit_struct
    seq tuple tuple_struct map struct identifier ignored_any
  }
}

fn deserialize_token<'de, V: Visitor<'de>>(
  parser: &mut JsoncParser<'de>,
  token: Token<'de>,
  visitor: V,
) -> Result<V::Value, ParseError> {
  let token_range = Range::new(parser.scanner.token_start(), parser.scanner.token_end());
  let text = parser.text;
  let result = match token {
    Token::Null => visitor.visit_unit(),
    Token::Boolean(b) => visitor.visit_bool(b),
    Token::Number(n) => visit_number(n, visitor),
    Token::String(s) => match s {
      Cow::Borrowed(b) => visitor.visit_borrowed_str(b),
      Cow::Owned(o) => visitor.visit_string(o),
    },
    Token::OpenBracket => {
      parser.enter_container()?;
      let result = visitor.visit_seq(ScannerSeqAccess { parser, first: true });
      parser.exit_container();
      result
    }
    Token::OpenBrace => {
      parser.enter_container()?;
      let result = visitor.visit_map(ScannerMapAccess { parser, first: true });
      parser.exit_container();
      result
    }
    other => return Err(parser.unexpected_token_error(&other)),
  };
  result.map_err(|e| e.with_position(token_range, text))
}

// number handling

fn visit_number<'de, V: Visitor<'de>>(raw: &str, visitor: V) -> Result<V::Value, ParseError> {
  // handle hexadecimal
  let trimmed = raw.trim_start_matches(['-', '+']);
  if trimmed.len() > 2 && (trimmed.starts_with("0x") || trimmed.starts_with("0X")) {
    let hex_part = &trimmed[2..];
    match i64::from_str_radix(hex_part, 16) {
      Ok(val) => {
        let val = if raw.starts_with('-') { -val } else { val };
        return visitor.visit_i64(val);
      }
      Err(_) => return visitor.visit_str(raw),
    }
  }

  // strip unary plus
  let num_str = raw.trim_start_matches('+');

  if let Ok(v) = num_str.parse::<i64>() {
    return visitor.visit_i64(v);
  }
  if let Ok(v) = num_str.parse::<u64>() {
    return visitor.visit_u64(v);
  }
  if let Ok(v) = num_str.parse::<f64>() {
    return visitor.visit_f64(v);
  }

  // fallback for unparseable numbers
  visitor.visit_str(raw)
}

// array handling

struct ScannerSeqAccess<'a, 'b> {
  parser: &'b mut JsoncParser<'a>,
  first: bool,
}

impl<'de, 'b> SeqAccess<'de> for ScannerSeqAccess<'de, 'b> {
  type Error = ParseError;

  fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error> {
    let token = if self.first {
      self.first = false;
      self.parser.scan()?
    } else {
      self.parser.scan_array_comma()?
    };

    match token {
      Some(Token::CloseBracket) => Ok(None),
      Some(token) => {
        self.parser.put_back(token);
        seed.deserialize(&mut *self.parser).map(Some)
      }
      None => Err(
        self
          .parser
          .scanner
          .create_error_for_current_token(ParseErrorKind::UnterminatedArray),
      ),
    }
  }
}

// object handling

struct ScannerMapAccess<'a, 'b> {
  parser: &'b mut JsoncParser<'a>,
  first: bool,
}

impl<'de, 'b> MapAccess<'de> for ScannerMapAccess<'de, 'b> {
  type Error = ParseError;

  fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error> {
    let key = self.parser.scan_object_entry(self.first)?;
    self.first = false;

    match key {
      None => Ok(None),
      Some(key) => {
        let key_str = key.into_string();
        seed
          .deserialize(<String as IntoDeserializer<Self::Error>>::into_deserializer(key_str))
          .map(Some)
      }
    }
  }

  fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Self::Error> {
    self.parser.scan_object_colon()?;
    seed.deserialize(&mut *self.parser)
  }
}

// enum handling

struct ObjectEnumAccess<'a, 'b> {
  parser: &'b mut JsoncParser<'a>,
  variant: String,
}

impl<'de, 'b> EnumAccess<'de> for ObjectEnumAccess<'de, 'b> {
  type Error = ParseError;
  type Variant = ObjectVariantAccess<'de, 'b>;

  fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error> {
    let variant = seed.deserialize(<String as IntoDeserializer<Self::Error>>::into_deserializer(
      self.variant,
    ))?;
    Ok((variant, ObjectVariantAccess { parser: self.parser }))
  }
}

struct ObjectVariantAccess<'a, 'b> {
  parser: &'b mut JsoncParser<'a>,
}

impl<'de, 'b> VariantAccess<'de> for ObjectVariantAccess<'de, 'b> {
  type Error = ParseError;

  fn unit_variant(self) -> Result<(), Self::Error> {
    ::serde::Deserialize::deserialize(&mut *self.parser)
  }

  fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Self::Error> {
    seed.deserialize(&mut *self.parser)
  }

  fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error> {
    match self.parser.scan()? {
      Some(Token::OpenBracket) => {
        self.parser.enter_container()?;
        let result = visitor.visit_seq(ScannerSeqAccess {
          parser: self.parser,
          first: true,
        });
        self.parser.exit_container();
        result
      }
      _ => Err(ParseError::custom_err(
        "expected an array for tuple variant".to_string(),
      )),
    }
  }

  fn struct_variant<V: Visitor<'de>>(
    self,
    _fields: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    match self.parser.scan()? {
      Some(Token::OpenBrace) => {
        self.parser.enter_container()?;
        let result = visitor.visit_map(ScannerMapAccess {
          parser: self.parser,
          first: true,
        });
        self.parser.exit_container();
        result
      }
      _ => Err(ParseError::custom_err(
        "expected an object for struct variant".to_string(),
      )),
    }
  }
}

#[cfg(test)]
mod tests {
  use pretty_assertions::assert_eq;
  use serde_json::Value as SerdeValue;
  use std::str::FromStr;

  use super::*;

  #[test]
  fn it_should_error_when_has_error() {
    assert_has_error(
      "[][]",
      "Text cannot contain more than one JSON value on line 1 column 3",
    );
  }

  fn assert_has_error(text: &str, message: &str) {
    let result = parse_to_serde_value::<SerdeValue>(text, &Default::default());
    match result {
      Ok(_) => panic!("Expected error, but did not find one."),
      Err(err) => assert_eq!(err.to_string(), message),
    }
  }

  #[test]
  fn it_should_parse_to_serde_value() {
    let result = parse_to_serde_value::<SerdeValue>(
      r#"{ "a": { "a1": 5 }, "b": [0.3e+025], "c": "c1", "d": true, "e": false, "f": null }"#,
      &Default::default(),
    )
    .unwrap();

    let mut expected_value = serde_json::map::Map::new();
    expected_value.insert("a".to_string(), {
      let mut inner_obj = serde_json::map::Map::new();
      inner_obj.insert(
        "a1".to_string(),
        SerdeValue::Number(serde_json::Number::from_str("5").unwrap()),
      );
      SerdeValue::Object(inner_obj)
    });
    expected_value.insert("b".to_string(), {
      let mut inner_array = Vec::new();
      inner_array.push(SerdeValue::Number(serde_json::Number::from_str("0.3e+025").unwrap()));
      SerdeValue::Array(inner_array)
    });
    expected_value.insert("c".to_string(), SerdeValue::String("c1".to_string()));
    expected_value.insert("d".to_string(), SerdeValue::Bool(true));
    expected_value.insert("e".to_string(), SerdeValue::Bool(false));
    expected_value.insert("f".to_string(), SerdeValue::Null);

    assert_eq!(result, SerdeValue::Object(expected_value));
  }

  #[test]
  fn it_should_parse_hexadecimal_numbers_to_decimal() {
    let result = parse_to_serde_value::<SerdeValue>(
      r#"{
        "hex1": 0x7DF,
        "hex2": 0xFF,
        "hex3": 0x10
      }"#,
      &Default::default(),
    )
    .unwrap();

    let mut expected_value = serde_json::map::Map::new();
    expected_value.insert("hex1".to_string(), SerdeValue::Number(serde_json::Number::from(2015)));
    expected_value.insert("hex2".to_string(), SerdeValue::Number(serde_json::Number::from(255)));
    expected_value.insert("hex3".to_string(), SerdeValue::Number(serde_json::Number::from(16)));

    assert_eq!(result, SerdeValue::Object(expected_value));
  }

  #[test]
  fn it_should_parse_unary_plus_numbers() {
    let result = parse_to_serde_value::<SerdeValue>(
      r#"{
        "pos1": +42,
        "pos2": +0.5,
        "pos3": +1e10
      }"#,
      &Default::default(),
    )
    .unwrap();

    let mut expected_value = serde_json::map::Map::new();
    expected_value.insert("pos1".to_string(), SerdeValue::Number(serde_json::Number::from(42)));
    expected_value.insert(
      "pos2".to_string(),
      SerdeValue::Number(serde_json::Number::from_str("0.5").unwrap()),
    );
    expected_value.insert(
      "pos3".to_string(),
      SerdeValue::Number(serde_json::Number::from_str("1e10").unwrap()),
    );

    assert_eq!(result, SerdeValue::Object(expected_value));
  }

  #[test]
  fn it_should_deserialize_to_struct() {
    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    struct Config {
      name: String,
      value: u32,
      enabled: bool,
    }

    let result: Config = parse_to_serde_value(
      r#"{ "name": "test", "value": 42, "enabled": true }"#,
      &Default::default(),
    )
    .unwrap();

    assert_eq!(
      result,
      Config {
        name: "test".to_string(),
        value: 42,
        enabled: true,
      }
    );
  }

  #[test]
  fn it_should_report_position_on_type_error() {
    #[derive(::serde::Deserialize, Debug)]
    #[serde(crate = "::serde")]
    #[allow(dead_code)]
    struct Config {
      name: String,
    }

    let text = r#"{
  "name": true
}"#;
    let err = parse_to_serde_value::<Config>(text, &Default::default()).unwrap_err();
    // the error should point at `true` (line 2, column 11)
    assert_eq!(err.line_display(), 2);
    assert_eq!(err.column_display(), 11);
    assert!(err.to_string().contains("invalid type"), "got: {}", err);
  }

  #[test]
  fn it_should_deserialize_option_fields() {
    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    struct Config {
      a: Option<u32>,
      b: Option<u32>,
    }

    let result: Config = parse_to_serde_value(r#"{ "a": 5, "b": null }"#, &Default::default()).unwrap();

    assert_eq!(result, Config { a: Some(5), b: None });
  }

  #[test]
  fn it_should_deserialize_enum() {
    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    enum Color {
      Red,
      Green,
      Blue,
    }

    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    struct Config {
      color: Color,
    }

    let result: Config = parse_to_serde_value(r#"{ "color": "Red" }"#, &Default::default()).unwrap();

    assert_eq!(result, Config { color: Color::Red });
  }

  #[test]
  fn it_should_deserialize_complex_enum() {
    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    enum Shape {
      Circle(f64),
      Rectangle { width: f64, height: f64 },
    }

    let result: Shape = parse_to_serde_value(r#"{ "Circle": 5.0 }"#, &Default::default()).unwrap();
    assert_eq!(result, Shape::Circle(5.0));

    let result: Shape = parse_to_serde_value(
      r#"{ "Rectangle": { "width": 3.0, "height": 4.0 } }"#,
      &Default::default(),
    )
    .unwrap();
    assert_eq!(
      result,
      Shape::Rectangle {
        width: 3.0,
        height: 4.0
      }
    );
  }

  #[test]
  fn it_should_return_null_for_empty_input() {
    let result = parse_to_serde_value::<SerdeValue>("", &Default::default()).unwrap();
    assert_eq!(result, SerdeValue::Null);
  }

  #[test]
  fn it_should_return_none_for_empty_input_with_option() {
    let result: Option<SerdeValue> = parse_to_serde_value("", &Default::default()).unwrap();
    assert_eq!(result, None);
  }

  #[test]
  fn it_should_return_some_for_non_empty_input_with_option() {
    let result: Option<SerdeValue> = parse_to_serde_value(r#"{ "a": 1 }"#, &Default::default()).unwrap();
    assert!(result.is_some());
  }

  #[test]
  fn it_should_return_none_for_empty_input_with_option_struct() {
    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    struct Config {
      value: u32,
    }

    let result: Option<Config> = parse_to_serde_value("", &Default::default()).unwrap();
    assert_eq!(result, None);

    let result: Option<Config> = parse_to_serde_value("  \n  ", &Default::default()).unwrap();
    assert_eq!(result, None);
  }

  #[test]
  fn it_should_handle_comments_in_jsonc() {
    #[derive(::serde::Deserialize, Debug, PartialEq)]
    #[serde(crate = "::serde")]
    struct Config {
      value: u32,
    }

    let result: Config = parse_to_serde_value(
      r#"{
        // this is a comment
        "value": 42 /* inline comment */
      }"#,
      &Default::default(),
    )
    .unwrap();

    assert_eq!(result, Config { value: 42 });
  }
}
