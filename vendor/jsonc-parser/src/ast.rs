use super::common::Range;
use super::common::Ranged;
use std::borrow::Cow;

/// JSON value.
#[derive(Debug, PartialEq, Clone)]
pub enum Value<'a> {
  StringLit(StringLit<'a>),
  NumberLit(NumberLit<'a>),
  BooleanLit(BooleanLit),
  Object(Object<'a>),
  Array(Array<'a>),
  NullKeyword(NullKeyword),
}

impl<'a> Value<'a> {
  pub fn as_string_lit(&self) -> Option<&StringLit<'a>> {
    match self {
      Value::StringLit(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_number_lit(&self) -> Option<&NumberLit<'a>> {
    match self {
      Value::NumberLit(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_boolean_lit(&self) -> Option<&BooleanLit> {
    match self {
      Value::BooleanLit(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_object(&self) -> Option<&Object<'a>> {
    match self {
      Value::Object(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_array(&self) -> Option<&Array<'a>> {
    match self {
      Value::Array(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_null_keyword(&self) -> Option<&NullKeyword> {
    match self {
      Value::NullKeyword(node) => Some(node),
      _ => None,
    }
  }
}

#[cfg(feature = "serde_json")]
impl<'a> From<Value<'a>> for serde_json::Value {
  fn from(value: Value<'a>) -> Self {
    use std::str::FromStr;
    match value {
      Value::Array(arr) => {
        let vec = arr.elements.into_iter().map(|v| v.into()).collect();
        serde_json::Value::Array(vec)
      }
      Value::BooleanLit(b) => serde_json::Value::Bool(b.value),
      Value::NullKeyword(_) => serde_json::Value::Null,
      Value::NumberLit(num) => {
        // check if this is a hexadecimal literal (0x or 0X prefix)
        let num_str = num.value.trim_start_matches(['-', '+']);
        if num_str.len() > 2 && (num_str.starts_with("0x") || num_str.starts_with("0X")) {
          // Parse hexadecimal and convert to decimal
          let hex_part = &num_str[2..];
          match i64::from_str_radix(hex_part, 16) {
            Ok(decimal_value) => {
              let final_value = if num.value.starts_with('-') {
                -decimal_value
              } else {
                decimal_value
              };
              serde_json::Value::Number(serde_json::Number::from(final_value))
            }
            Err(_) => serde_json::Value::String(num.value.to_string()),
          }
        } else {
          // standard decimal number
          let num_for_parsing = num.value.trim_start_matches('+');
          match serde_json::Number::from_str(num_for_parsing) {
            Ok(number) => serde_json::Value::Number(number),
            Err(_) => serde_json::Value::String(num.value.to_string()),
          }
        }
      }
      Value::Object(obj) => {
        let mut map = serde_json::map::Map::new();
        for prop in obj.properties {
          map.insert(prop.name.into_string(), prop.value.into());
        }
        serde_json::Value::Object(map)
      }
      Value::StringLit(s) => serde_json::Value::String(s.value.into_owned()),
    }
  }
}

/// Node that can appear in the AST.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Node<'a, 'b> {
  StringLit(&'b StringLit<'a>),
  NumberLit(&'b NumberLit<'a>),
  BooleanLit(&'b BooleanLit),
  Object(&'b Object<'a>),
  ObjectProp(&'b ObjectProp<'a>),
  Array(&'b Array<'a>),
  NullKeyword(&'b NullKeyword),
  WordLit(&'b WordLit<'a>),
}

impl<'a, 'b> Node<'a, 'b> {
  /// Gets the node kind.
  pub fn kind(&self) -> NodeKind {
    match self {
      Node::StringLit(_) => NodeKind::StringLit,
      Node::NumberLit(_) => NodeKind::NumberLit,
      Node::BooleanLit(_) => NodeKind::BooleanLit,
      Node::Object(_) => NodeKind::Object,
      Node::ObjectProp(_) => NodeKind::ObjectProp,
      Node::Array(_) => NodeKind::Array,
      Node::NullKeyword(_) => NodeKind::NullKeyword,
      Node::WordLit(_) => NodeKind::WordLit,
    }
  }

  pub fn as_string_lit(&self) -> Option<&'b StringLit<'a>> {
    match self {
      Node::StringLit(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_number_lit(&self) -> Option<&'b NumberLit<'a>> {
    match self {
      Node::NumberLit(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_boolean_lit(&self) -> Option<&'b BooleanLit> {
    match self {
      Node::BooleanLit(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_object(&self) -> Option<&'b Object<'a>> {
    match self {
      Node::Object(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_object_prop(&self) -> Option<&'b ObjectProp<'a>> {
    match self {
      Node::ObjectProp(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_array(&self) -> Option<&'b Array<'a>> {
    match self {
      Node::Array(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_null_keyword(&self) -> Option<&'b NullKeyword> {
    match self {
      Node::NullKeyword(node) => Some(node),
      _ => None,
    }
  }

  pub fn as_word_lit(&self) -> Option<&'b WordLit<'a>> {
    match self {
      Node::WordLit(node) => Some(node),
      _ => None,
    }
  }
}

/// Kind of AST node.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum NodeKind {
  StringLit,
  NumberLit,
  BooleanLit,
  Object,
  ObjectProp,
  Array,
  NullKeyword,
  WordLit,
}

/// Node surrounded in double quotes (ex. `"my string"`).
#[derive(Debug, PartialEq, Clone)]
pub struct StringLit<'a> {
  pub range: Range,
  pub value: Cow<'a, str>,
}

/// A string that's not in quotes.
/// Usually the appearance of this would be a parsing error.
#[derive(Debug, PartialEq, Clone)]
pub struct WordLit<'a> {
  pub range: Range,
  pub value: &'a str,
}

/// Represents a number (ex. `123`, `99.99`, `-1.2e+2`).
#[derive(Debug, PartialEq, Clone)]
pub struct NumberLit<'a> {
  pub range: Range,
  pub value: &'a str,
}

/// Represents a boolean (ex. `true` or `false`).
#[derive(Debug, PartialEq, Clone)]
pub struct BooleanLit {
  pub range: Range,
  pub value: bool,
}

/// Represents the null keyword (ex. `null`).
#[derive(Debug, PartialEq, Clone)]
pub struct NullKeyword {
  pub range: Range,
}

/// Represents an object that may contain properties (ex. `{}`, `{ "prop": 4 }`).
#[derive(Debug, PartialEq, Clone)]
pub struct Object<'a> {
  pub range: Range,
  pub properties: Vec<ObjectProp<'a>>,
}

macro_rules! generate_take {
  ($self:ident, $name:ident, $value_type:ident) => {
    // there must be some better code that could be written here...
    if let Some(pos) = $self.properties.iter().position(|p| p.name.as_str() == $name) {
      if let Value::$value_type(_) = &$self.properties[pos].value {
        if let Value::$value_type(node) = $self.properties.remove(pos).value {
          Some(node)
        } else {
          None
        }
      } else {
        None
      }
    } else {
      None
    }
  };
}

macro_rules! generate_get {
  ($self:ident, $name:ident, $value_type:ident) => {
    $self
      .properties
      .iter()
      .filter(|p| p.name.as_str() == $name)
      .map(|p| {
        if let Value::$value_type(node) = &p.value {
          Some(node)
        } else {
          None
        }
      })
      .next()
      .flatten()
  };
}

impl<'a> Object<'a> {
  /// Gets a property value in the object by its name.
  pub fn get(&self, name: &str) -> Option<&ObjectProp<'a>> {
    self.properties.iter().find(|p| p.name.as_str() == name)
  }

  /// Gets a string property value from the object by name.
  /// Returns `None` when not a string or it doesn't exist.
  pub fn get_string(&self, name: &str) -> Option<&StringLit<'a>> {
    generate_get!(self, name, StringLit)
  }

  /// Gets a number property value from the object by name.
  /// Returns `None` when not a number or it doesn't exist.
  pub fn get_number(&self, name: &str) -> Option<&NumberLit<'a>> {
    generate_get!(self, name, NumberLit)
  }

  /// Gets a boolean property value from the object by name.
  /// Returns `None` when not a boolean or it doesn't exist.
  pub fn get_boolean(&self, name: &str) -> Option<&BooleanLit> {
    generate_get!(self, name, BooleanLit)
  }

  /// Gets an object property value from the object by name.
  /// Returns `None` when not an object or it doesn't exist.
  pub fn get_object(&self, name: &str) -> Option<&Object<'a>> {
    generate_get!(self, name, Object)
  }

  /// Gets an array property value from the object by name.
  /// Returns `None` when not an array or it doesn't exist.
  pub fn get_array(&self, name: &str) -> Option<&Array<'a>> {
    generate_get!(self, name, Array)
  }

  /// Takes a value from the object by name.
  /// Returns `None` when it doesn't exist.
  pub fn take(&mut self, name: &str) -> Option<ObjectProp<'a>> {
    if let Some(pos) = self.properties.iter().position(|p| p.name.as_str() == name) {
      Some(self.properties.remove(pos))
    } else {
      None
    }
  }

  /// Takes a string property value from the object by name.
  /// Returns `None` when not a string or it doesn't exist.
  pub fn take_string(&mut self, name: &str) -> Option<StringLit<'a>> {
    generate_take!(self, name, StringLit)
  }

  /// Takes a number property value from the object by name.
  /// Returns `None` when not a number or it doesn't exist.
  pub fn take_number(&mut self, name: &str) -> Option<NumberLit<'a>> {
    generate_take!(self, name, NumberLit)
  }

  /// Takes a boolean property value from the object by name.
  /// Returns `None` when not a boolean or it doesn't exist.
  pub fn take_boolean(&mut self, name: &str) -> Option<BooleanLit> {
    generate_take!(self, name, BooleanLit)
  }

  /// Takes an object property value from the object by name.
  /// Returns `None` when not an object or it doesn't exist.
  pub fn take_object(&mut self, name: &str) -> Option<Object<'a>> {
    generate_take!(self, name, Object)
  }

  /// Takes an array property value from the object by name.
  /// Returns `None` when not an array or it doesn't exist.
  pub fn take_array(&mut self, name: &str) -> Option<Array<'a>> {
    generate_take!(self, name, Array)
  }
}

/// Represents an object property (ex. `"prop": []`).
#[derive(Debug, PartialEq, Clone)]
pub struct ObjectProp<'a> {
  pub range: Range,
  pub name: ObjectPropName<'a>,
  pub value: Value<'a>,
}

/// Represents an object property name that may or may not be in quotes.
#[derive(Debug, PartialEq, Clone)]
pub enum ObjectPropName<'a> {
  String(StringLit<'a>),
  Word(WordLit<'a>),
}

impl<'a> ObjectPropName<'a> {
  /// Converts the object property name into a string.
  pub fn into_string(self) -> String {
    match self {
      ObjectPropName::String(lit) => lit.value.into_owned(),
      ObjectPropName::Word(lit) => lit.value.to_string(),
    }
  }

  /// Gets the object property name as a string reference.
  pub fn as_str(&'a self) -> &'a str {
    match self {
      ObjectPropName::String(lit) => lit.value.as_ref(),
      ObjectPropName::Word(lit) => lit.value,
    }
  }
}

/// Represents an array that may contain elements (ex. `[]`, `[5, 6]`).
#[derive(Debug, PartialEq, Clone)]
pub struct Array<'a> {
  pub range: Range,
  pub elements: Vec<Value<'a>>,
}

/// Kind of JSONC comment.
#[derive(Debug, PartialEq, Clone)]
pub enum CommentKind {
  Line,
  Block,
}

/// JSONC comment.
#[derive(Debug, PartialEq, Clone)]
pub enum Comment<'a> {
  Line(CommentLine<'a>),
  Block(CommentBlock<'a>),
}

impl<'a> Comment<'a> {
  /// Gets the text of the comment.
  pub fn text(&self) -> &'a str {
    match self {
      Comment::Line(line) => line.text,
      Comment::Block(line) => line.text,
    }
  }

  /// Gets the comment kind.
  pub fn kind(&self) -> CommentKind {
    match self {
      Comment::Line(_) => CommentKind::Line,
      Comment::Block(_) => CommentKind::Block,
    }
  }
}

impl<'a> Ranged for Comment<'a> {
  fn range(&self) -> Range {
    match self {
      Comment::Line(line) => line.range(),
      Comment::Block(line) => line.range(),
    }
  }
}

/// Represents a comment line (ex. `// my comment`).
#[derive(Debug, PartialEq, Clone)]
pub struct CommentLine<'a> {
  pub range: Range,
  pub text: &'a str,
}

/// Represents a comment block (ex. `/* my comment */`).
#[derive(Debug, PartialEq, Clone)]
pub struct CommentBlock<'a> {
  pub range: Range,
  pub text: &'a str,
}

// Object Property Name

impl<'a, 'b> From<&'b ObjectPropName<'a>> for Node<'a, 'b> {
  fn from(object_prop_name: &'b ObjectPropName<'a>) -> Node<'a, 'b> {
    match object_prop_name {
      ObjectPropName::String(lit) => lit.into(),
      ObjectPropName::Word(lit) => lit.into(),
    }
  }
}

impl<'a> Ranged for ObjectPropName<'a> {
  fn range(&self) -> Range {
    match self {
      ObjectPropName::String(lit) => lit.range(),
      ObjectPropName::Word(lit) => lit.range(),
    }
  }
}

// Implement Traits

macro_rules! impl_ranged {
  ($($node_name:ident),*) => {
    $(
      impl Ranged for $node_name {
        fn range(&self) -> Range {
            self.range
        }
      }
    )*
  };
}

impl_ranged![BooleanLit, NullKeyword];

macro_rules! impl_ranged_lifetime {
  ($($node_name:ident),*) => {
    $(
      impl<'a> Ranged for $node_name<'a> {
        fn range(&self) -> Range {
            self.range
        }
      }
    )*
  };
}

impl_ranged_lifetime![
  WordLit,
  Object,
  ObjectProp,
  Array,
  CommentLine,
  CommentBlock,
  NumberLit,
  StringLit
];

impl<'a> Ranged for Value<'a> {
  fn range(&self) -> Range {
    match self {
      Value::Array(node) => node.range(),
      Value::BooleanLit(node) => node.range(),
      Value::NullKeyword(node) => node.range(),
      Value::NumberLit(node) => node.range(),
      Value::Object(node) => node.range(),
      Value::StringLit(node) => node.range(),
    }
  }
}

impl<'a, 'b> Ranged for Node<'a, 'b> {
  fn range(&self) -> Range {
    match self {
      Node::StringLit(node) => node.range(),
      Node::NumberLit(node) => node.range(),
      Node::BooleanLit(node) => node.range(),
      Node::NullKeyword(node) => node.range(),
      Node::WordLit(node) => node.range(),
      Node::Array(node) => node.range(),
      Node::Object(node) => node.range(),
      Node::ObjectProp(node) => node.range(),
    }
  }
}

macro_rules! generate_node {
    ($($node_name:ident),*) => {
        $(
        impl<'a, 'b> From<&'b $node_name> for Node<'a, 'b> {
            fn from(node: &'b $node_name) -> Node<'a, 'b> {
                Node::$node_name(node)
            }
        }
        )*
    };
}

generate_node![BooleanLit, NullKeyword];

macro_rules! generate_node_lifetime {
    ($($node_name:ident),*) => {

        $(
        impl<'a, 'b> From<&'b $node_name<'a>> for Node<'a, 'b> {
            fn from(node: &'b $node_name<'a>) -> Node<'a, 'b> {
                Node::$node_name(node)
            }
        }
        )*
    };
}

generate_node_lifetime![WordLit, Object, ObjectProp, Array, NumberLit, StringLit];

impl<'a, 'b> From<&'b Value<'a>> for Node<'a, 'b> {
  fn from(value: &'b Value<'a>) -> Node<'a, 'b> {
    match value {
      Value::Array(node) => Node::Array(node),
      Value::BooleanLit(node) => Node::BooleanLit(node),
      Value::NullKeyword(node) => Node::NullKeyword(node),
      Value::NumberLit(node) => Node::NumberLit(node),
      Value::Object(node) => Node::Object(node),
      Value::StringLit(node) => Node::StringLit(node),
    }
  }
}

#[cfg(test)]
mod test {
  use super::*;
  use crate::ParseOptions;
  use crate::parse_to_ast;

  #[test]
  fn it_should_take() {
    let ast = parse_to_ast(
      "{'prop': 'asdf', 'other': 'text'}",
      &Default::default(),
      &ParseOptions::default(),
    )
    .unwrap();
    let mut obj = match ast.value {
      Some(Value::Object(obj)) => obj,
      _ => unreachable!(),
    };

    assert_eq!(obj.properties.len(), 2);
    assert_eq!(obj.take_string("asdf"), None);
    assert_eq!(obj.properties.len(), 2);
    assert_eq!(obj.take_number("prop"), None);
    assert_eq!(obj.properties.len(), 2);
    assert!(obj.take_string("prop").is_some());
    assert_eq!(obj.properties.len(), 1);
    assert_eq!(obj.take("something"), None);
    assert_eq!(obj.properties.len(), 1);
    assert!(obj.take("other").is_some());
    assert_eq!(obj.properties.len(), 0);
  }

  #[test]
  fn it_should_get() {
    let ast = parse_to_ast("{'prop': 'asdf'}", &Default::default(), &ParseOptions::default()).unwrap();
    let obj = match ast.value {
      Some(Value::Object(obj)) => obj,
      _ => unreachable!(),
    };

    assert_eq!(obj.properties.len(), 1);
    assert_eq!(obj.get_string("asdf"), None);
    assert!(obj.get_string("prop").is_some());
    assert_eq!(obj.get("asdf"), None);
    assert_eq!(obj.properties.len(), 1);
  }

  #[cfg(feature = "serde_json")]
  #[test]
  fn it_should_coerce_to_serde_value() {
    let ast = parse_to_ast(
      r#"{"prop":[true,1,null,"str"]}"#,
      &Default::default(),
      &ParseOptions::default(),
    )
    .unwrap();
    let value = ast.value.unwrap();
    let serde_value: serde_json::Value = value.into();

    assert_eq!(
      serde_value,
      serde_json::json!({
        "prop": [
          true,
          1,
          null,
          "str"
        ]
      })
    );
  }

  #[cfg(feature = "serde_json")]
  #[test]
  fn handle_weird_data() {
    let ast = parse_to_ast(
      r#"{eyyyyyyy:6yy:6000e000615yyyk:6}"#,
      &Default::default(),
      &ParseOptions::default(),
    )
    .unwrap();
    let value = ast.value.unwrap();
    let serde_value: serde_json::Value = value.into();

    assert_eq!(
      serde_value,
      // this output is fine because the input is bad
      serde_json::json!({
        "eyyyyyyy": 6,
        "yy": "6000e000615",
        "yyyk": 6
      })
    );
  }
}
