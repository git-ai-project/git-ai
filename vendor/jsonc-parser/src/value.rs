use core::slice::Iter;
use std::borrow::Cow;

/// A JSON value.
#[derive(Clone, PartialEq, Debug)]
pub enum JsonValue<'a> {
  String(Cow<'a, str>),
  Number(&'a str),
  Boolean(bool),
  Object(JsonObject<'a>),
  Array(JsonArray<'a>),
  Null,
}

#[cfg(not(feature = "preserve_order"))]
pub type Map<K, V> = std::collections::HashMap<K, V>;
#[cfg(feature = "preserve_order")]
pub type Map<K, V> = indexmap::IndexMap<K, V>;

/// A JSON object.
#[derive(Clone, PartialEq, Debug)]
pub struct JsonObject<'a>(Map<String, JsonValue<'a>>);

impl<'a> IntoIterator for JsonObject<'a> {
  type Item = (String, JsonValue<'a>);
  #[cfg(not(feature = "preserve_order"))]
  type IntoIter = std::collections::hash_map::IntoIter<String, JsonValue<'a>>;
  #[cfg(feature = "preserve_order")]
  type IntoIter = indexmap::map::IntoIter<String, JsonValue<'a>>;

  fn into_iter(self) -> Self::IntoIter {
    self.0.into_iter()
  }
}

impl<'a> From<Map<String, JsonValue<'a>>> for JsonObject<'a> {
  fn from(properties: Map<String, JsonValue>) -> JsonObject {
    JsonObject::new(properties)
  }
}

#[cfg(not(feature = "preserve_order"))]
#[inline(always)]
fn remove_entry<'a>(map: &mut Map<String, JsonValue<'a>>, key: &str) -> Option<(String, JsonValue<'a>)> {
  map.remove_entry(key)
}

#[cfg(feature = "preserve_order")]
#[inline(always)]
fn remove_entry<'a>(map: &mut Map<String, JsonValue<'a>>, key: &str) -> Option<(String, JsonValue<'a>)> {
  map.shift_remove_entry(key)
}

macro_rules! generate_take {
  ($self:ident, $name:ident, $value_type:ident) => {
    match remove_entry(&mut $self.0, $name) {
      Some((_, JsonValue::$value_type(value))) => Some(value),
      Some((key, value)) => {
        // add it back
        $self.0.insert(key, value);
        None
      }
      _ => None,
    }
  };
}

macro_rules! generate_get {
  ($self:ident, $name:ident, $value_type:ident) => {
    match $self.0.get($name) {
      Some(JsonValue::$value_type(value)) => Some(value),
      _ => None,
    }
  };
}

impl<'a> JsonObject<'a> {
  /// Creates a new JsonObject.
  pub fn new(inner: Map<String, JsonValue<'a>>) -> JsonObject<'a> {
    JsonObject(inner)
  }

  /// Creates a new JsonObject with the specified capacity.
  pub fn with_capacity(capacity: usize) -> JsonObject<'a> {
    JsonObject(Map::with_capacity(capacity))
  }

  /// Drops the object returning the inner map.
  pub fn take_inner(self) -> Map<String, JsonValue<'a>> {
    self.0
  }

  /// Gets the number of properties.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Gets if there are no properties.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  /// Gets a value in the object by its name.
  pub fn get(&self, name: &str) -> Option<&JsonValue<'a>> {
    self.0.get(name)
  }

  /// Gets a string property value from the object by name.
  /// Returns `None` when not a string or it doesn't exist.
  pub fn get_string(&self, name: &str) -> Option<&Cow<'a, str>> {
    generate_get!(self, name, String)
  }

  /// Gets a number property value from the object by name.
  /// Returns `None` when not a number or it doesn't exist.
  pub fn get_number(&self, name: &str) -> Option<&'a str> {
    generate_get!(self, name, Number)
  }

  /// Gets a boolean property value from the object by name.
  /// Returns `None` when not a boolean or it doesn't exist.
  pub fn get_boolean(&self, name: &str) -> Option<bool> {
    let result = generate_get!(self, name, Boolean);
    result.cloned()
  }

  /// Gets an object property value from the object by name.
  /// Returns `None` when not an object or it doesn't exist.
  pub fn get_object(&self, name: &str) -> Option<&JsonObject<'a>> {
    generate_get!(self, name, Object)
  }

  /// Gets an array property value from the object by name.
  /// Returns `None` when not an array or it doesn't exist.
  pub fn get_array(&self, name: &str) -> Option<&JsonArray<'a>> {
    generate_get!(self, name, Array)
  }

  /// Takes a value from the object by name.
  /// Returns `None` when it doesn't exist.
  pub fn take(&mut self, name: &str) -> Option<JsonValue<'a>> {
    remove_entry(&mut self.0, name).map(|(_, value)| value)
  }

  /// Takes a string property value from the object by name.
  /// Returns `None` when not a string or it doesn't exist.
  pub fn take_string(&mut self, name: &str) -> Option<Cow<'a, str>> {
    generate_take!(self, name, String)
  }

  /// Takes a number property value from the object by name.
  /// Returns `None` when not a number or it doesn't exist.
  pub fn take_number(&mut self, name: &str) -> Option<&'a str> {
    generate_take!(self, name, Number)
  }

  /// Takes a boolean property value from the object by name.
  /// Returns `None` when not a boolean or it doesn't exist.
  pub fn take_boolean(&mut self, name: &str) -> Option<bool> {
    generate_take!(self, name, Boolean)
  }

  /// Takes an object property value from the object by name.
  /// Returns `None` when not an object or it doesn't exist.
  pub fn take_object(&mut self, name: &str) -> Option<JsonObject<'a>> {
    generate_take!(self, name, Object)
  }

  /// Takes an array property value from the object by name.
  /// Returns `None` when not an array or it doesn't exist.
  pub fn take_array(&mut self, name: &str) -> Option<JsonArray<'a>> {
    generate_take!(self, name, Array)
  }
}

/// A JSON array.
#[derive(Clone, PartialEq, Debug)]
pub struct JsonArray<'a>(Vec<JsonValue<'a>>);

impl<'a> IntoIterator for JsonArray<'a> {
  type Item = JsonValue<'a>;
  type IntoIter = std::vec::IntoIter<Self::Item>;

  fn into_iter(self) -> Self::IntoIter {
    self.0.into_iter()
  }
}

impl<'a> From<Vec<JsonValue<'a>>> for JsonArray<'a> {
  fn from(elements: Vec<JsonValue<'a>>) -> JsonArray<'a> {
    JsonArray::new(elements)
  }
}

impl<'a> JsonArray<'a> {
  /// Creates a new JsonArray.
  pub fn new(inner: Vec<JsonValue<'a>>) -> JsonArray<'a> {
    JsonArray(inner)
  }

  /// Drops the object returning the inner vector.
  pub fn take_inner(self) -> Vec<JsonValue<'a>> {
    self.0
  }

  /// Iterates over the array elements.
  pub fn iter(&self) -> Iter<'_, JsonValue<'a>> {
    self.0.iter()
  }

  /// Gets a value from the array by index.
  pub fn get(&self, index: usize) -> Option<&JsonValue<'a>> {
    self.0.get(index)
  }

  /// Gets the number of elements.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Gets if the array is empty.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn it_should_take() {
    let mut inner = Map::new();
    inner.insert(String::from("prop"), JsonValue::String(Cow::Borrowed("asdf")));
    inner.insert(String::from("other"), JsonValue::String(Cow::Borrowed("text")));
    let mut obj = JsonObject::new(inner);

    assert_eq!(obj.len(), 2);
    assert_eq!(obj.take_string("asdf"), None);
    assert_eq!(obj.len(), 2);
    assert_eq!(obj.take_number("prop"), None);
    assert_eq!(obj.len(), 2);
    assert_eq!(obj.take_string("prop"), Some(Cow::Borrowed("asdf")));
    assert_eq!(obj.len(), 1);
    assert_eq!(obj.take("something"), None);
    assert_eq!(obj.len(), 1);
    assert_eq!(obj.take("other"), Some(JsonValue::String(Cow::Borrowed("text"))));
    assert_eq!(obj.len(), 0);
  }

  #[test]
  fn it_should_get() {
    let mut inner = Map::new();
    inner.insert(String::from("prop"), JsonValue::String(Cow::Borrowed("asdf")));
    let obj = JsonObject::new(inner);

    assert_eq!(obj.len(), 1);
    assert_eq!(obj.get_string("asdf"), None);
    assert_eq!(obj.get_string("prop"), Some(&Cow::Borrowed("asdf")));
    assert_eq!(obj.get("prop"), Some(&JsonValue::String(Cow::Borrowed("asdf"))));
    assert_eq!(obj.get("asdf"), None);
    assert_eq!(obj.len(), 1);
  }
}
