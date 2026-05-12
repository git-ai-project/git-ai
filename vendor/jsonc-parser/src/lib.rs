//! # jsonc-parser
//!
//! A JSON parser and manipulator that supports comments and other JSON extensions.
//!
//! ## Parsing
//!
//! To a simple `JsonValue`:
//!
//! ```
//! use jsonc_parser::parse_to_value;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let json_value = parse_to_value(r#"{ "test": 5 } // test"#, &Default::default())?;
//! // check the json_value here
//! # Ok(())
//! # }
//! ```
//!
//! Or an AST:
//!
//! ```
//! use jsonc_parser::parse_to_ast;
//! use jsonc_parser::CollectOptions;
//! use jsonc_parser::CommentCollectionStrategy;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let parse_result = parse_to_ast(r#"{ "test": 5 } // test"#, &CollectOptions {
//!     comments: CommentCollectionStrategy::Separate, // include comments in result
//!     tokens: true, // include tokens in result
//! }, &Default::default())?;
//! // ...inspect parse_result for value, tokens, and comments here...
//! # Ok(())
//! # }
//! ```
//!
//! ## Manipulation (CST)
//!
//! When enabling the `cst` cargo feature, parsing to a CST provides a first class manipulation API:
//!
//! ```
//! # #[cfg(feature = "cst")]
//! # {
//! use jsonc_parser::cst::CstRootNode;
//! use jsonc_parser::ParseOptions;
//! use jsonc_parser::json;
//!
//! let json_text = r#"{
//!   // comment
//!   "data": 123
//! }"#;
//!
//! let root = CstRootNode::parse(json_text, &ParseOptions::default()).unwrap();
//! let root_obj = root.object_value_or_set();
//!
//! root_obj.get("data").unwrap().set_value(json!({
//!   "nested": true
//! }));
//! root_obj.append("new_key", json!([456, 789, false]));
//!
//! assert_eq!(root.to_string(), r#"{
//!   // comment
//!   "data": {
//!     "nested": true
//!   },
//!   "new_key": [456, 789, false]
//! }"#);
//! # }
//! ```
//!
//! ## Serde
//!
//! If you enable the `"serde"` feature as follows:
//!
//! ```toml
//! # in Cargo.toml
//! jsonc-parser = { version = "...", features = ["serde"] }
//! ```
//!
//! Then you can use the `parse_to_serde_value` function to deserialize JSONC directly into
//! any type implementing `serde::Deserialize`:
//!
//! ```
//! # #[cfg(feature = "serde")]
//! # {
//! use jsonc_parser::parse_to_serde_value;
//!
//! #[derive(serde::Deserialize)]
//! struct Config {
//!   test: u32,
//! }
//!
//! # fn parse_example() -> Result<(), Box<dyn std::error::Error>> {
//! let config: Config = parse_to_serde_value(r#"{ "test": 5 } // test"#, &Default::default())?;
//! # Ok(())
//! # }
//! # }
//! ```
//!
//! ## Parse Strictly as JSON
//!
//! By default this library is extremely loose in what it allows parsing. To be strict,
//! provide `ParseOptions` and set all the options to false:
//!
//! ```
//! use jsonc_parser::parse_to_value;
//! use jsonc_parser::ParseOptions;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let text = "{}";
//! let json_value = parse_to_value(text, &ParseOptions {
//!   allow_comments: false,
//!   allow_loose_object_property_names: false,
//!   allow_trailing_commas: false,
//!   allow_missing_commas: false,
//!   allow_single_quoted_strings: false,
//!   allow_hexadecimal_numbers: false,
//!   allow_unary_plus_numbers: false,
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Error column number with unicode-width
//!
//! To get more accurate display column numbers in error messages, enable the `error_unicode_width` cargo feature,
//! which will pull in and use the [unicode-width](https://crates.io/crates/unicode-width) dependency internally.
//! Otherwise it will use the character count, which isn't as accurate of a number, but will probably be good enough
//! in most cases.

#![deny(clippy::print_stderr)]
#![deny(clippy::print_stdout)]
#![allow(clippy::uninlined_format_args)]

pub mod ast;
pub mod common;
#[cfg(feature = "cst")]
pub mod cst;
pub mod errors;
mod parse_to_ast;
mod parse_to_value;
mod parser;
mod scanner;
#[cfg(feature = "serde")]
mod serde;
mod string;
pub mod tokens;
mod value;

pub use parse_to_ast::*;
pub use parse_to_value::*;
pub use scanner::*;
pub use string::ParseStringErrorKind;
pub use value::*;

#[cfg(feature = "serde")]
pub use serde::*;
