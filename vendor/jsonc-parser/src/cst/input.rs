/// API user provided value for inserts and replaces.
#[derive(Debug, Clone)]
pub enum CstInputValue {
  Null,
  Bool(bool),
  Number(String),
  String(String),
  Array(Vec<CstInputValue>),
  Object(Vec<(String, CstInputValue)>),
}

impl CstInputValue {
  pub(crate) fn force_multiline(&self) -> bool {
    match self {
      CstInputValue::Null | CstInputValue::Bool(_) | CstInputValue::Number(_) | CstInputValue::String(_) => false,
      CstInputValue::Array(v) => v.iter().any(|v| v.is_object_or_array_with_elements()),
      CstInputValue::Object(v) => !v.is_empty(),
    }
  }

  fn is_object_or_array_with_elements(&self) -> bool {
    match self {
      CstInputValue::Null | CstInputValue::Bool(_) | CstInputValue::Number(_) | CstInputValue::String(_) => false,
      CstInputValue::Array(v) => !v.is_empty(),
      CstInputValue::Object(v) => !v.is_empty(),
    }
  }
}

impl From<bool> for CstInputValue {
  fn from(b: bool) -> Self {
    CstInputValue::Bool(b)
  }
}

impl From<&str> for CstInputValue {
  fn from(s: &str) -> Self {
    CstInputValue::String(s.to_string())
  }
}

impl From<String> for CstInputValue {
  fn from(s: String) -> Self {
    CstInputValue::String(s)
  }
}

impl From<f64> for CstInputValue {
  fn from(n: f64) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl From<usize> for CstInputValue {
  fn from(n: usize) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl From<isize> for CstInputValue {
  fn from(n: isize) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl From<u64> for CstInputValue {
  fn from(n: u64) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl From<i64> for CstInputValue {
  fn from(n: i64) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl From<u32> for CstInputValue {
  fn from(n: u32) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl From<i32> for CstInputValue {
  fn from(n: i32) -> Self {
    CstInputValue::Number(n.to_string())
  }
}

impl<T> From<Vec<T>> for CstInputValue
where
  T: Into<CstInputValue>,
{
  fn from(vec: Vec<T>) -> Self {
    CstInputValue::Array(vec.into_iter().map(Into::into).collect())
  }
}

impl From<Vec<(String, CstInputValue)>> for CstInputValue {
  fn from(obj: Vec<(String, CstInputValue)>) -> Self {
    CstInputValue::Object(obj)
  }
}

#[macro_export]
macro_rules! json {
  (null) => {
    $crate::cst::CstInputValue::Null
  };

  ([ $($elems:tt),* $(,)? ]) => {
    $crate::cst::CstInputValue::Array(vec![
      $(json!($elems)),*
    ])
  };

  ({ $($key:tt : $value:tt),* $(,)? }) => {
    $crate::cst::CstInputValue::Object(vec![
      $(
         ($crate::json!(private_quote_property $key).to_string(), json!($value))
      ),*
    ])
  };

  ($other:expr) => {
    $crate::cst::CstInputValue::from($other)
  };

  // hack to not have another public macro for quoting object key properties
  (private_quote_property $key:ident) => {
    stringify!($key)
  };

  (private_quote_property $key:expr) => {
    $key
  };
}
