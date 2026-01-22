//! Position-encoded array abstraction.
//! Converts plain Rust structs to/from sparse JSON objects.
//!
//! Uses `Option<Option<T>>` semantics for the three states:
//! - `None` = not-set (key omitted from sparse object)
//! - `Some(None)` = null (explicit null value)
//! - `Some(Some(v))` = value

use super::types::SparseArray;
use serde_json::Value;

/// Field type for position-encoded values.
/// - `None` = not-set (key omitted from sparse object)
/// - `Some(None)` = null (explicit null value)
/// - `Some(Some(v))` = value
pub type PosField<T> = Option<Option<T>>;

/// Trait for types that can be position-encoded.
pub trait PosEncoded: Sized + Default {
    fn to_sparse(&self) -> SparseArray;
    fn from_sparse(arr: &SparseArray) -> Self;
}

/// Convert a PosField<String> to JSON Value for sparse array.
/// Returns None for not-set (key should be omitted).
pub fn string_to_json(field: &PosField<String>) -> Option<Value> {
    match field {
        None => None,
        Some(None) => Some(Value::Null),
        Some(Some(s)) => Some(Value::String(s.clone())),
    }
}

/// Convert a PosField<u32> to JSON Value for sparse array.
pub fn u32_to_json(field: &PosField<u32>) -> Option<Value> {
    match field {
        None => None,
        Some(None) => Some(Value::Null),
        Some(Some(n)) => Some(Value::Number((*n).into())),
    }
}

/// Convert a PosField<u64> to JSON Value for sparse array.
pub fn u64_to_json(field: &PosField<u64>) -> Option<Value> {
    match field {
        None => None,
        Some(None) => Some(Value::Null),
        Some(Some(n)) => Some(Value::Number((*n).into())),
    }
}

/// Get a string field from a sparse array at a position.
pub fn sparse_get_string(arr: &SparseArray, pos: usize) -> PosField<String> {
    match arr.get(&pos.to_string()) {
        None => None, // not-set
        Some(Value::Null) => Some(None), // explicit null
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => None, // wrong type, treat as not-set
    }
}

/// Get a u32 field from a sparse array at a position.
pub fn sparse_get_u32(arr: &SparseArray, pos: usize) -> PosField<u32> {
    match arr.get(&pos.to_string()) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Number(n)) => n.as_u64().and_then(|v| {
            if v <= u32::MAX as u64 {
                Some(Some(v as u32))
            } else {
                None
            }
        }),
        Some(_) => None,
    }
}

/// Get a u64 field from a sparse array at a position.
pub fn sparse_get_u64(arr: &SparseArray, pos: usize) -> PosField<u64> {
    match arr.get(&pos.to_string()) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Number(n)) => n.as_u64().map(|v| Some(v)),
        Some(_) => None,
    }
}

/// Convert a PosField<Vec<String>> to JSON array.
pub fn vec_string_to_json(field: &PosField<Vec<String>>) -> Option<Value> {
    match field {
        None => None,
        Some(None) => Some(Value::Null),
        Some(Some(vec)) => Some(Value::Array(
            vec.iter().map(|s| Value::String(s.clone())).collect(),
        )),
    }
}

/// Convert a PosField<Vec<u32>> to JSON array.
pub fn vec_u32_to_json(field: &PosField<Vec<u32>>) -> Option<Value> {
    match field {
        None => None,
        Some(None) => Some(Value::Null),
        Some(Some(vec)) => Some(Value::Array(
            vec.iter().map(|n| Value::Number((*n).into())).collect(),
        )),
    }
}

/// Convert a PosField<Vec<u64>> to JSON array.
pub fn vec_u64_to_json(field: &PosField<Vec<u64>>) -> Option<Value> {
    match field {
        None => None,
        Some(None) => Some(Value::Null),
        Some(Some(vec)) => Some(Value::Array(
            vec.iter().map(|n| Value::Number((*n).into())).collect(),
        )),
    }
}

/// Get a Vec<String> field from a sparse array at a position.
pub fn sparse_get_vec_string(arr: &SparseArray, pos: usize) -> PosField<Vec<String>> {
    match arr.get(&pos.to_string()) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Array(arr)) => {
            let strings: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Some(Some(strings))
        }
        Some(_) => None,
    }
}

/// Get a Vec<u32> field from a sparse array at a position.
pub fn sparse_get_vec_u32(arr: &SparseArray, pos: usize) -> PosField<Vec<u32>> {
    match arr.get(&pos.to_string()) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Array(arr)) => {
            let nums: Vec<u32> = arr
                .iter()
                .filter_map(|v| v.as_u64().and_then(|n| {
                    if n <= u32::MAX as u64 {
                        Some(n as u32)
                    } else {
                        None
                    }
                }))
                .collect();
            Some(Some(nums))
        }
        Some(_) => None,
    }
}

/// Get a Vec<u64> field from a sparse array at a position.
pub fn sparse_get_vec_u64(arr: &SparseArray, pos: usize) -> PosField<Vec<u64>> {
    match arr.get(&pos.to_string()) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Array(arr)) => {
            let nums: Vec<u64> = arr.iter().filter_map(|v| v.as_u64()).collect();
            Some(Some(nums))
        }
        Some(_) => None,
    }
}

/// Set a value in a sparse array at a position.
/// If value is Some, inserts; if None, does nothing (not-set).
pub fn sparse_set(arr: &mut SparseArray, pos: usize, value: Option<Value>) {
    if let Some(v) = value {
        arr.insert(pos.to_string(), v);
    }
}

/// Macro to define position-encoded structs with minimal boilerplate.
/// Generates: struct with PosField<T> fields, Default, builder methods, to_sparse, from_sparse
#[macro_export]
macro_rules! pos_encoded {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                #[pos($pos:expr)]
                $field_vis:vis $field:ident : String
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default)]
        $vis struct $name {
            $( $field_vis $field: $crate::metrics::pos_encoded::PosField<String>, )*
        }

        impl $name {
            pub fn new() -> Self {
                Self::default()
            }

            $(
                pub fn $field(mut self, value: impl Into<String>) -> Self {
                    self.$field = Some(Some(value.into()));
                    self
                }

                paste::paste! {
                    pub fn [<$field _null>](mut self) -> Self {
                        self.$field = Some(None);
                        self
                    }
                }
            )*
        }

        impl $crate::metrics::pos_encoded::PosEncoded for $name {
            fn to_sparse(&self) -> $crate::metrics::types::SparseArray {
                let mut map = $crate::metrics::types::SparseArray::new();
                $(
                    $crate::metrics::pos_encoded::sparse_set(
                        &mut map,
                        $pos,
                        $crate::metrics::pos_encoded::string_to_json(&self.$field)
                    );
                )*
                map
            }

            fn from_sparse(arr: &$crate::metrics::types::SparseArray) -> Self {
                Self {
                    $( $field: $crate::metrics::pos_encoded::sparse_get_string(arr, $pos), )*
                }
            }
        }
    };
}

/// Macro variant for structs with mixed field types.
/// Use this for event values that have u32 fields.
#[macro_export]
macro_rules! pos_encoded_values {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                #[pos($pos:expr)]
                $field_vis:vis $field:ident : $ty:tt
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default)]
        $vis struct $name {
            $( $field_vis $field: $crate::metrics::pos_encoded::PosField<$ty>, )*
        }

        impl $name {
            pub fn new() -> Self {
                Self::default()
            }

            $(
                $crate::metrics::pos_encoded::impl_builder!($field, $ty);
            )*
        }

        impl $crate::metrics::pos_encoded::PosEncoded for $name {
            fn to_sparse(&self) -> $crate::metrics::types::SparseArray {
                let mut map = $crate::metrics::types::SparseArray::new();
                $(
                    $crate::metrics::pos_encoded::sparse_set(
                        &mut map,
                        $pos,
                        $crate::metrics::pos_encoded::to_json_typed!(&self.$field, $ty)
                    );
                )*
                map
            }

            fn from_sparse(arr: &$crate::metrics::types::SparseArray) -> Self {
                Self {
                    $( $field: $crate::metrics::pos_encoded::from_sparse_typed!(arr, $pos, $ty), )*
                }
            }
        }
    };
}

/// Helper macro to implement builder methods based on type.
#[macro_export]
macro_rules! impl_builder {
    ($field:ident, String) => {
        pub fn $field(mut self, value: impl Into<String>) -> Self {
            self.$field = Some(Some(value.into()));
            self
        }

        paste::paste! {
            pub fn [<$field _null>](mut self) -> Self {
                self.$field = Some(None);
                self
            }
        }
    };
    ($field:ident, u32) => {
        pub fn $field(mut self, value: u32) -> Self {
            self.$field = Some(Some(value));
            self
        }

        paste::paste! {
            pub fn [<$field _null>](mut self) -> Self {
                self.$field = Some(None);
                self
            }
        }
    };
    ($field:ident, u64) => {
        pub fn $field(mut self, value: u64) -> Self {
            self.$field = Some(Some(value));
            self
        }

        paste::paste! {
            pub fn [<$field _null>](mut self) -> Self {
                self.$field = Some(None);
                self
            }
        }
    };
}

/// Helper macro to convert field to JSON based on type.
#[macro_export]
macro_rules! to_json_typed {
    ($field:expr, String) => {
        $crate::metrics::pos_encoded::string_to_json($field)
    };
    ($field:expr, u32) => {
        $crate::metrics::pos_encoded::u32_to_json($field)
    };
    ($field:expr, u64) => {
        $crate::metrics::pos_encoded::u64_to_json($field)
    };
}

/// Helper macro to get field from sparse array based on type.
#[macro_export]
macro_rules! from_sparse_typed {
    ($arr:expr, $pos:expr, String) => {
        $crate::metrics::pos_encoded::sparse_get_string($arr, $pos)
    };
    ($arr:expr, $pos:expr, u32) => {
        $crate::metrics::pos_encoded::sparse_get_u32($arr, $pos)
    };
    ($arr:expr, $pos:expr, u64) => {
        $crate::metrics::pos_encoded::sparse_get_u64($arr, $pos)
    };
}

// Re-export helper macros for use by other modules
pub use crate::from_sparse_typed;
pub use crate::impl_builder;
pub use crate::pos_encoded;
pub use crate::pos_encoded_values;
pub use crate::to_json_typed;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_to_json() {
        assert_eq!(string_to_json(&None), None);
        assert_eq!(string_to_json(&Some(None)), Some(Value::Null));
        assert_eq!(
            string_to_json(&Some(Some("test".to_string()))),
            Some(Value::String("test".to_string()))
        );
    }

    #[test]
    fn test_u32_to_json() {
        assert_eq!(u32_to_json(&None), None);
        assert_eq!(u32_to_json(&Some(None)), Some(Value::Null));
        assert_eq!(u32_to_json(&Some(Some(42))), Some(Value::Number(42.into())));
    }

    #[test]
    fn test_sparse_get_string() {
        let mut arr = SparseArray::new();
        assert_eq!(sparse_get_string(&arr, 0), None);

        arr.insert("0".to_string(), Value::Null);
        assert_eq!(sparse_get_string(&arr, 0), Some(None));

        arr.insert("1".to_string(), Value::String("test".to_string()));
        assert_eq!(sparse_get_string(&arr, 1), Some(Some("test".to_string())));
    }

    #[test]
    fn test_sparse_get_u32() {
        let mut arr = SparseArray::new();
        assert_eq!(sparse_get_u32(&arr, 0), None);

        arr.insert("0".to_string(), Value::Null);
        assert_eq!(sparse_get_u32(&arr, 0), Some(None));

        arr.insert("1".to_string(), Value::Number(42.into()));
        assert_eq!(sparse_get_u32(&arr, 1), Some(Some(42)));
    }
}
