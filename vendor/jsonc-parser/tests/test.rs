extern crate jsonc_parser;

use jsonc_parser::ast::*;
use jsonc_parser::common::*;
use jsonc_parser::*;
use pretty_assertions::assert_eq;
use std::fs::{self};
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

#[test]
fn test_specs() {
  for json_path in get_json_file_paths_in_dir(Path::new("./tests/specs")) {
    let text_file_path = json_path.with_extension("txt");
    let json_file_text = fs::read_to_string(&json_path).unwrap().replace("\r\n", "\n");
    let result = parse_to_ast(
      &json_file_text,
      &CollectOptions {
        comments: CommentCollectionStrategy::Separate,
        tokens: true,
      },
      &Default::default(),
    )
    .expect("Expected no error.");
    let result_text = parse_result_to_test_str(&result);
    let expected_text = fs::read_to_string(&text_file_path).unwrap().replace("\r\n", "\n");
    // fs::write(&text_file_path, result_text.clone()).unwrap();
    assert_eq!(result_text.trim(), expected_text.trim());
  }
}

#[cfg(feature = "cst")]
#[test]
fn test_cst() {
  for json_path in get_json_file_paths_in_dir(Path::new("./tests/specs")) {
    let json_file_text = fs::read_to_string(&json_path).unwrap().replace("\r\n", "\n");

    eprintln!("Parsing: {:?}", json_path);
    let value = jsonc_parser::cst::CstRootNode::parse(&json_file_text, &ParseOptions::default()).unwrap();
    let cst_string = value.to_string();
    assert_eq!(cst_string, json_file_text);
  }
}

fn get_json_file_paths_in_dir(path: &Path) -> Vec<PathBuf> {
  return read_dir_recursively(path);

  fn read_dir_recursively(dir_path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();

    for entry in dir_path.read_dir().expect("read dir failed") {
      if let Ok(entry) = entry {
        let entry_path = entry.path();
        if entry_path.is_file() {
          if let Some(ext) = entry_path.extension() {
            if ext == "json" {
              result.push(entry_path);
            }
          }
        } else {
          result.extend(read_dir_recursively(&entry_path));
        }
      }
    }

    result
  }
}

// todo: move elsewhere and improve

fn parse_result_to_test_str(parse_result: &ParseResult) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str(&format!(
    "  \"value\": {},\n",
    match &parse_result.value {
      Some(value) => value_to_test_str(value).replace("\n", "\n  "),
      None => String::from("null"),
    }
  ));
  text.push_str("  \"comments\": [");
  let comments = parse_result.comments.as_ref().expect("Expected comments.");
  let collection_count = comments.len();
  let mut comments = comments.iter().collect::<Vec<_>>();
  comments.sort_by(|a, b| a.0.cmp(b.0));
  for (i, comment_collection) in comments.into_iter().enumerate() {
    text.push_str("\n    ");
    text.push_str(&comments_to_test_str(comment_collection).replace("\n", "\n    "));
    if i + 1 < collection_count {
      text.push(',');
    }
  }
  text.push_str("\n  ]\n");
  text.push_str("}\n");
  text
}

fn value_to_test_str(value: &Value) -> String {
  match value {
    Value::StringLit(lit) => string_lit_to_test_str(lit),
    Value::NumberLit(lit) => number_lit_to_test_str(lit),
    Value::BooleanLit(lit) => boolean_lit_to_test_str(lit),
    Value::Object(obj) => object_to_test_str(obj),
    Value::Array(arr) => array_to_test_str(arr),
    Value::NullKeyword(keyword) => null_keyword_to_test_str(keyword),
  }
}

fn range_to_test_str(range: Range) -> String {
  let mut text = String::new();
  text.push_str("\"range\": {\n");
  text.push_str(&format!("  \"start\": {},\n", range.start));
  text.push_str(&format!("  \"end\": {},\n", range.end));
  text.push('}');
  text
}

fn string_lit_to_test_str(lit: &StringLit) -> String {
  lit_to_test_str("string", &lit.value, lit.range)
}

fn word_lit_to_test_str(lit: &WordLit) -> String {
  lit_to_test_str("word", lit.value, lit.range)
}

fn number_lit_to_test_str(lit: &NumberLit) -> String {
  lit_to_test_str("number", lit.value, lit.range)
}

fn boolean_lit_to_test_str(lit: &BooleanLit) -> String {
  lit_to_test_str("boolean", &lit.value.to_string(), lit.range)
}

fn lit_to_test_str(lit_type: &str, value: &str, range: Range) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str(&format!("  \"type\": \"{}\",\n", lit_type));
  text.push_str(&format!("  {},\n", range_to_test_str(range).replace("\n", "\n  ")));
  text.push_str(&format!("  \"value\": \"{}\"\n", escape_json_str(value)));
  text.push('}');
  text
}

fn object_to_test_str(obj: &Object) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str("  \"type\": \"object\",\n");
  text.push_str(&format!("  {},\n", range_to_test_str(obj.range).replace("\n", "\n  ")));
  text.push_str("  \"properties\": [");
  let prop_count = obj.properties.len();
  for (i, prop) in obj.properties.iter().enumerate() {
    text.push_str("\n    ");
    text.push_str(&object_prop_to_test_str(prop).replace("\n", "\n    "));
    if i + 1 < prop_count {
      text.push(',');
    }
  }
  text.push_str("\n  ]\n");
  text.push('}');
  text
}

fn object_prop_to_test_str(obj_prop: &ObjectProp) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str("  \"type\": \"objectProp\",\n");
  text.push_str(&format!(
    "  {},\n",
    range_to_test_str(obj_prop.range).replace("\n", "\n  ")
  ));
  text.push_str(&format!(
    "  \"name\": {},\n",
    object_prop_name_to_test_str(&obj_prop.name).replace("\n", "\n  ")
  ));
  text.push_str(&format!(
    "  \"value\": {}\n",
    value_to_test_str(&obj_prop.value).replace("\n", "\n  ")
  ));
  text.push('}');
  text
}

fn object_prop_name_to_test_str(obj_prop_name: &ObjectPropName) -> String {
  match obj_prop_name {
    ObjectPropName::String(lit) => string_lit_to_test_str(lit),
    ObjectPropName::Word(word) => word_lit_to_test_str(word),
  }
}

fn array_to_test_str(arr: &Array) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str("  \"type\": \"array\",\n");
  text.push_str(&format!("  {},\n", range_to_test_str(arr.range).replace("\n", "\n  ")));
  text.push_str("  \"elements\": [");
  let elements_count = arr.elements.len();
  for (i, element) in arr.elements.iter().enumerate() {
    text.push_str("\n    ");
    text.push_str(&value_to_test_str(element).replace("\n", "\n    "));
    if i + 1 < elements_count {
      text.push(',');
    }
  }
  text.push_str("\n  ]\n");
  text.push('}');
  text
}

fn null_keyword_to_test_str(null_keyword: &NullKeyword) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str("  \"type\": \"null\",\n");
  text.push_str(&format!(
    "  {}\n",
    range_to_test_str(null_keyword.range).replace("\n", "\n  ")
  ));
  text.push('}');
  text
}

fn comments_to_test_str(comments: (&usize, &Rc<Vec<Comment>>)) -> String {
  let mut text = String::new();
  text.push_str("{\n");
  text.push_str(&format!("  \"pos\": {},\n", comments.0));
  text.push_str("  \"comments\": [");
  let comments_count = comments.1.len();
  for (i, comment) in comments.1.iter().enumerate() {
    text.push_str("\n    ");
    text.push_str(&comment_to_test_str(comment).replace("\n", "\n    "));
    if i + 1 < comments_count {
      text.push(',');
    }
  }
  text.push_str("\n  ]\n");
  text.push('}');
  text
}

fn comment_to_test_str(comment: &Comment) -> String {
  match comment {
    Comment::Line(line) => comment_line_to_test_str(line),
    Comment::Block(block) => comment_block_to_test_str(block),
  }
}

fn comment_line_to_test_str(line: &CommentLine) -> String {
  lit_to_test_str("line", line.text, line.range)
}

fn comment_block_to_test_str(block: &CommentBlock) -> String {
  lit_to_test_str("block", block.text, block.range)
}

fn escape_json_str(text: &str) -> String {
  text
    .replace("\\", "\\\\")
    .replace("\x08", "\\b")
    .replace("\x0C", "\\f")
    .replace("\r", "\\r")
    .replace("\t", "\\t")
    .replace("\n", "\\n")
}
