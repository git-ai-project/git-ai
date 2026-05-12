#![feature(test)]

extern crate test;

use jsonc_parser::parse_to_ast;
use jsonc_parser::parse_to_value;
use std::fs::read_to_string;
use test::Bencher;

#[bench]
fn citm_catalog_json_large_ast(b: &mut Bencher) {
  bench_ast(b, &get_citm_catalog_json_large());
}

#[bench]
fn citm_catalog_json_large_value(b: &mut Bencher) {
  bench_value(b, &get_citm_catalog_json_large());
}

#[bench]
#[cfg(feature = "serde")]
fn citm_catalog_json_large_serde(b: &mut Bencher) {
  bench_serde(b, &get_citm_catalog_json_large());
}

#[bench]
#[cfg(feature = "serde")]
fn citm_catalog_json_large_serde_value(b: &mut Bencher) {
  bench_serde_value(b, &get_citm_catalog_json_large());
}

#[bench]
fn tsconfig_json_ast(b: &mut Bencher) {
  bench_ast(b, &get_tsconfig_json());
}

#[bench]
fn tsconfig_json_value(b: &mut Bencher) {
  bench_value(b, &get_tsconfig_json());
}

#[bench]
#[cfg(feature = "serde")]
fn tsconfig_json_serde_value(b: &mut Bencher) {
  bench_serde_value(b, &get_tsconfig_json());
}

#[bench]
fn package_json_ast(b: &mut Bencher) {
  bench_ast(b, &get_package_json());
}

#[bench]
fn package_json_value(b: &mut Bencher) {
  bench_value(b, &get_package_json());
}

#[bench]
#[cfg(feature = "serde")]
fn package_json_serde_value(b: &mut Bencher) {
  bench_serde_value(b, &get_package_json());
}

// bench helpers

fn bench_ast(b: &mut Bencher, json_text: &str) {
  b.iter(|| parse_to_ast(json_text, &Default::default(), &Default::default()).unwrap());
}

fn bench_value(b: &mut Bencher, json_text: &str) {
  b.iter(|| parse_to_value(json_text, &Default::default()).unwrap());
}

#[cfg(feature = "serde")]
fn bench_serde(b: &mut Bencher, json_text: &str) {
  b.iter(|| serde_json::from_str::<serde_json::Value>(json_text).unwrap());
}

#[cfg(feature = "serde")]
fn bench_serde_value(b: &mut Bencher, json_text: &str) {
  b.iter(|| jsonc_parser::parse_to_serde_value::<serde_json::Value>(json_text, &Default::default()).unwrap());
}

// data

fn get_citm_catalog_json_large() -> String {
  create_json_array_of_object(&get_citm_catalog_json(), 6)
}

fn get_citm_catalog_json() -> String {
  // from https://github.com/serde-rs/json-benchmark/blob/master/data/citm_catalog.json
  read_to_string("benches/data/citm_catalog.json").unwrap()
}

fn get_tsconfig_json() -> String {
  read_to_string("benches/data/tsconfig.json").unwrap()
}

fn get_package_json() -> String {
  read_to_string("benches/data/package.txt").unwrap()
}

fn create_json_array_of_object(text: &str, length: usize) -> String {
  let mut result = String::new();
  result.push_str("[");
  for i in 0..length {
    if i > 0 {
      result.push_str(",");
    }
    result.push_str(text);
  }
  result.push_str("]");
  result
}
