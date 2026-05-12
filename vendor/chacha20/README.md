# RustCrypto: ChaCha20

[![Crate][crate-image]][crate-link]
[![Docs][docs-image]][docs-link]
[![Build Status][build-image]][build-link]
![Apache2/MIT licensed][license-image]
![Rust Version][rustc-image]
[![Project Chat][chat-image]][chat-link]
[![HAZMAT][hazmat-image]][hazmat-link]

Pure Rust implementation of the [ChaCha20 Stream Cipher][1].

<img src="https://raw.githubusercontent.com/RustCrypto/meta/master/img/stream-ciphers/chacha20.png" width="300px">

## About

[ChaCha20][1] is a [stream cipher][2] which is designed to support
high-performance software implementations.

It improves upon the previous [Salsa20][3] stream cipher with increased
per-round diffusion at no cost to performance.

This crate also contains an implementation of [XChaCha20][4]: a variant
of ChaCha20 with an extended 192-bit (24-byte) nonce, gated under the
`chacha20` Cargo feature (on-by-default).

## Implementations

This crate contains the following implementations of ChaCha20, all of which
work on stable Rust with the following `RUSTFLAGS`:

- `x86` / `x86_64`
  - `avx2`: (~1.4cpb) `-Ctarget-cpu=haswell -Ctarget-feature=+avx2`
  - `sse2`: (~1.6cpb) `-Ctarget-feature=+sse2` (on by default on x86 CPUs)
  - `avx512`: `-Ctarget-feature=+avx512f,+avx512vl --cfg chacha20_avx512` requires Rust 1.89+
- `aarch64`
  - `neon` (~2-3x faster than `soft`) requires the `neon` feature enabled
- Portable
  - `soft`: (~5 cpb on x86/x86_64)

NOTE: cpb = cycles per byte (smaller is better)

## Security

### ⚠️ Warning: [Hazmat!][hazmat-link]

This crate does not ensure ciphertexts are authentic (i.e. by using a MAC to
verify ciphertext integrity), which can lead to serious vulnerabilities
if used incorrectly!

To avoid this, use an [AEAD][5] mode based on ChaCha20, i.e. [ChaCha20Poly1305][6].
See the [RustCrypto/AEADs][7] repository for more information.

USE AT YOUR OWN RISK!

### Notes

This crate has received one [security audit by NCC Group][8], with no significant
findings. We would like to thank [MobileCoin][9] for funding the audit.

All implementations contained in the crate (along with the underlying ChaCha20
stream cipher itself) are designed to execute in constant time.

## License

Licensed under either of:

- [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)
- [MIT license](http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

[//]: # (badges)

[crate-image]: https://img.shields.io/crates/v/chacha20.svg
[crate-link]: https://crates.io/crates/chacha20
[docs-image]: https://docs.rs/chacha20/badge.svg
[docs-link]: https://docs.rs/chacha20/
[build-image]: https://github.com/RustCrypto/stream-ciphers/actions/workflows/chacha20.yml/badge.svg
[build-link]: https://github.com/RustCrypto/stream-ciphers/actions/workflows/chacha20.yml
[license-image]: https://img.shields.io/badge/license-Apache2.0/MIT-blue.svg
[rustc-image]: https://img.shields.io/badge/rustc-1.85+-blue.svg
[chat-image]: https://img.shields.io/badge/zulip-join_chat-blue.svg
[chat-link]: https://rustcrypto.zulipchat.com/#narrow/stream/260049-stream-ciphers
[hazmat-image]: https://img.shields.io/badge/crypto-hazmat%E2%9A%A0-red.svg
[hazmat-link]: https://github.com/RustCrypto/meta/blob/master/HAZMAT.md

[//]: # (footnotes)

[1]: https://en.wikipedia.org/wiki/Salsa20#ChaCha_variant
[2]: https://en.wikipedia.org/wiki/Stream_cipher
[3]: https://en.wikipedia.org/wiki/Salsa20
[4]: https://tools.ietf.org/html/draft-arciszewski-xchacha-02
[5]: https://en.wikipedia.org/wiki/Authenticated_encryption
[6]: https://github.com/RustCrypto/AEADs/tree/master/chacha20poly1305
[7]: https://github.com/RustCrypto/AEADs
[8]: https://web.archive.org/web/20240108154854/https://research.nccgroup.com/wp-content/uploads/2020/02/NCC_Group_MobileCoin_RustCrypto_AESGCM_ChaCha20Poly1305_Implementation_Review_2020-02-12_v1.0.pdf
[9]: https://www.mobilecoin.com/
