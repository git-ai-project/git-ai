# widestring
[![crates.io](https://img.shields.io/crates/v/widestring.svg)](https://crates.io/crates/widestring) [![Documentation](https://docs.rs/widestring/badge.svg)](https://docs.rs/widestring) [![License](https://img.shields.io/crates/l/widestring)](https://github.com/VoidStarKat/widestring-rs#license) [![Build status](https://github.com/VoidStarKat/widestring-rs/actions/workflows/rust.yml/badge.svg?branch=main&event=push)](https://github.com/VoidStarKat/widestring-rs/actions)

A wide string Rust library for converting to and from wide strings, such as those often used in
Windows API or other FFI libaries. Both `u16` and `u32` string types are provided, including support
for UTF-16 and UTF-32, malformed encoding, C-style strings, etc.

Macros for converting string literals to UTF-16 and UTF-32 strings at compile time are also
included.

*Requires Rust 1.58 or greater.* If you need support for older versions of Rust, use 0.x versions of
this crate.

## Documentation

- [Crate API Reference](https://docs.rs/widestring)
- [Latest Changes](CHANGELOG.md)

### Optional Features

- **`alloc`** - Enabled by default. Enable use of the [`alloc`](https://doc.rust-lang.org/alloc/)
  crate when not using the `std` library.

  This enables the owned string types and aliases.

- **`std`** - Enabled by default. Enable features that depend on the Rust `std` library, including
  everything in the `alloc` feature.

- **`debugger_visualizer`** Add debugger visualizer data for crate types. _Requires Rust 1.71 or
  newer_

## License

All files in this library are dual-licensed and distributed under the terms of either of:

* [MIT License](LICENSE-MIT)
  ([https://opensource.org/license/mit](https://opensource.org/license/mit))
* [Apache License, Version 2.0](LICENSE-APACHE)
  ([https://www.apache.org/licenses/LICENSE-2.0](https://www.apache.org/licenses/LICENSE-2.0))

at your option.

### Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the
work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
