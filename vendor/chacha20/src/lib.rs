#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/RustCrypto/media/8f1a9894/logo.svg",
    html_favicon_url = "https://raw.githubusercontent.com/RustCrypto/media/8f1a9894/logo.svg"
)]

//! # Usage
//!
//! Cipher functionality is accessed using traits from re-exported [`cipher`] crate, or as a set
//! of random number generator types ending in `*Rng` which implement traits from the [`rand_core`]
//! crate.
//!
//! This crate contains the following variants of the ChaCha20 core algorithm:
//!
//! - [`ChaCha20`]: standard IETF variant with 96-bit nonce
//! - [`ChaCha8`] / [`ChaCha12`]: reduced round variants of ChaCha20
//! - [`XChaCha20`]: 192-bit extended nonce variant
//! - [`XChaCha8`] / [`XChaCha12`]: reduced round variants of XChaCha20
//! - [`ChaCha20Legacy`]: "djb" variant with 64-bit nonce.
//! **WARNING:** This implementation internally uses 32-bit counter,
//! while the original implementation uses 64-bit counter. In other words,
//! it does not allow encryption of more than 256 GiB of data.
//!
//! ## Example
#![cfg_attr(feature = "cipher", doc = " ```")]
#![cfg_attr(not(feature = "cipher"), doc = " ```ignore")]
//! use chacha20::ChaCha20;
//! // Import relevant traits
//! use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
//! use hex_literal::hex;
//!
//! let key = [0x42; 32];
//! let nonce = [0x24; 12];
//! let plaintext = hex!("00010203 04050607 08090A0B 0C0D0E0F");
//! let ciphertext = hex!("e405626e 4f1236b3 670ee428 332ea20e");
//!
//! // Key and IV must be references to the `Array` type.
//! // Here we use the `Into` trait to convert arrays into it.
//! let mut cipher = ChaCha20::new(&key.into(), &nonce.into());
//!
//! let mut buffer = plaintext.clone();
//!
//! // apply keystream (encrypt)
//! cipher.apply_keystream(&mut buffer);
//! assert_eq!(buffer, ciphertext);
//!
//! let ciphertext = buffer.clone();
//!
//! // ChaCha ciphers support seeking
//! cipher.seek(0u32);
//!
//! // decrypt ciphertext by applying keystream again
//! cipher.apply_keystream(&mut buffer);
//! assert_eq!(buffer, plaintext);
//!
//! // stream ciphers can be used with streaming messages
//! cipher.seek(0u32);
//! for chunk in buffer.chunks_mut(3) {
//!     cipher.apply_keystream(chunk);
//! }
//! assert_eq!(buffer, ciphertext);
//! ```
//!
//! # Configuration Flags
//!
//! You can modify crate using the following configuration flags:
//!
//! - `chacha20_backend="avx2"`: force AVX2 backend on x86/x86_64 targets.
//!   Requires enabled AVX2 target feature. Ignored on non-x86(_64) targets.
//! - `chacha20_backend="avx512": force AVX-512 backend on x86/x86_64 targets.
//!   Requires enabled AVX-512 target feature (MSRV 1.89). Ignored on non-x86(_64) targets.
//! - `chacha20_backend="soft"`: force software backend.
//! - `chacha20_backend="sse2"`: force SSE2 backend on x86/x86_64 targets.
//!   Requires enabled SSE2 target feature. Ignored on non-x86(-64) targets.
//!
//! The flags can be enabled using `RUSTFLAGS` environmental variable
//! (e.g. `RUSTFLAGS='--cfg chacha20_backend="avx2"'`) or by modifying `.cargo/config.toml`:
//!
//! ```toml
//! # In .cargo/config.toml
//! [build]
//! rustflags = ['--cfg', 'chacha20_backend="avx2"']
//! ```
//!
//! ## AVX-512 support
//!
//! To use the MSRV 1.89 AVX-512 support, you must enable it using: `--cfg chacha20_avx512`.
//!
//! [ChaCha]: https://tools.ietf.org/html/rfc8439
//! [Salsa]: https://en.wikipedia.org/wiki/Salsa20
//! [`chacha20poly1305`]: https://docs.rs/chacha20poly1305

pub mod variants;

mod backends;
#[cfg(feature = "cipher")]
mod chacha;
#[cfg(feature = "legacy")]
mod legacy;
#[cfg(feature = "rng")]
mod rng;
#[cfg(feature = "xchacha")]
mod xchacha;

#[cfg(feature = "cipher")]
pub use chacha::{ChaCha8, ChaCha12, ChaCha20, Key, KeyIvInit};
#[cfg(feature = "cipher")]
pub use cipher;
#[cfg(feature = "legacy")]
pub use legacy::{ChaCha20Legacy, LegacyNonce};
#[cfg(feature = "rng")]
pub use rand_core;
#[cfg(feature = "rng")]
pub use rng::{ChaCha8Rng, ChaCha12Rng, ChaCha20Rng, Seed, SerializedRngState};
#[cfg(feature = "xchacha")]
pub use xchacha::{XChaCha8, XChaCha12, XChaCha20, XNonce, hchacha};

use cfg_if::cfg_if;
use core::{fmt, marker::PhantomData};
use variants::Variant;

#[cfg(feature = "cipher")]
use cipher::{BlockSizeUser, StreamCipherCore, StreamCipherSeekCore, consts::U64};
#[cfg(feature = "zeroize")]
use zeroize::{Zeroize, ZeroizeOnDrop};

/// State initialization constant ("expand 32-byte k")
#[cfg(any(feature = "cipher", feature = "rng"))]
const CONSTANTS: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// Number of 32-bit words in the ChaCha state
const STATE_WORDS: usize = 16;

/// Marker type for a number of ChaCha rounds to perform.
pub trait Rounds: Copy {
    /// The amount of rounds to perform
    const COUNT: usize;
}

/// 8-rounds
#[derive(Copy, Clone, Debug)]
pub struct R8;

impl Rounds for R8 {
    const COUNT: usize = 4;
}

/// 12-rounds
#[derive(Copy, Clone, Debug)]
pub struct R12;

impl Rounds for R12 {
    const COUNT: usize = 6;
}

/// 20-rounds
#[derive(Copy, Clone, Debug)]
pub struct R20;

impl Rounds for R20 {
    const COUNT: usize = 10;
}

cfg_if! {
    if #[cfg(chacha20_backend = "soft")] {
        type Tokens = ();
    } else if #[cfg(any(target_arch = "x86", target_arch = "x86_64"))] {
        cfg_if! {
            if #[cfg(all(chacha20_avx512, chacha20_backend = "avx512"))] {
                #[cfg(not(all(target_feature = "avx512f", target_feature = "avx512vl")))]
                compile_error!("You must enable `avx512f` and `avx512vl` target features with \
                    `chacha20_backend = "avx512"` configuration option");
                type Tokens = ();
            } else if #[cfg(chacha20_backend = "avx2")] {
                #[cfg(not(target_feature = "avx2"))]
                compile_error!("You must enable `avx2` target feature with \
                    `chacha20_backend = "avx2"` configuration option");
                type Tokens = ();
            } else if #[cfg(chacha20_backend = "sse2")] {
                #[cfg(not(target_feature = "sse2"))]
                compile_error!("You must enable `sse2` target feature with \
                    `chacha20_backend = "sse2"` configuration option");
                type Tokens = ();
            } else {
                #[cfg(chacha20_avx512)]
                cpufeatures::new!(avx512_cpuid, "avx512f", "avx512vl");
                cpufeatures::new!(avx2_cpuid, "avx2");
                cpufeatures::new!(sse2_cpuid, "sse2");
                #[cfg(chacha20_avx512)]
                type Tokens = (avx512_cpuid::InitToken, avx2_cpuid::InitToken, sse2_cpuid::InitToken);
                #[cfg(not(chacha20_avx512))]
                type Tokens = (avx2_cpuid::InitToken, sse2_cpuid::InitToken);
            }
        }
    } else {
        type Tokens = ();
    }
}

/// The ChaCha core function.
pub struct ChaChaCore<R: Rounds, V: Variant> {
    /// Internal state of the core function
    state: [u32; STATE_WORDS],
    /// CPU target feature tokens
    #[allow(dead_code)]
    tokens: Tokens,
    /// Number of rounds to perform and the cipher variant
    _pd: PhantomData<(R, V)>,
}

impl<R: Rounds, V: Variant> ChaChaCore<R, V> {
    /// Constructs a ChaChaCore with the specified `key` and `iv`.
    ///
    /// You must ensure that the iv is of the correct size when using this method
    /// directly.
    ///
    /// # Panics
    /// If `iv.len()` is not equal to 4, 8, or 12.
    #[must_use]
    #[cfg(any(feature = "cipher", feature = "rng"))]
    fn new_internal(key: &[u8; 32], iv: &[u8]) -> Self {
        assert!(matches!(iv.len(), 4 | 8 | 12));

        let mut state = [0u32; STATE_WORDS];

        let ctr_size = size_of::<V::Counter>() / size_of::<u32>();
        let (const_dst, state_rem) = state.split_at_mut(4);
        let (key_dst, state_rem) = state_rem.split_at_mut(8);
        let (_ctr_dst, iv_dst) = state_rem.split_at_mut(ctr_size);

        const_dst.copy_from_slice(&CONSTANTS);

        // TODO(tarcieri): when MSRV 1.88, use `[T]::as_chunks` to avoid panic
        #[allow(clippy::unwrap_used, reason = "MSRV TODO")]
        {
            for (src, dst) in key.chunks_exact(4).zip(key_dst) {
                *dst = u32::from_le_bytes(src.try_into().unwrap());
            }

            assert_eq!(size_of_val(iv_dst), size_of_val(iv));
            for (src, dst) in iv.chunks_exact(4).zip(iv_dst) {
                *dst = u32::from_le_bytes(src.try_into().unwrap());
            }
        }

        cfg_if! {
            if #[cfg(chacha20_backend = "soft")] {
                let tokens = ();
            } else if #[cfg(any(target_arch = "x86", target_arch = "x86_64"))] {
                cfg_if! {
                    if #[cfg(chacha20_backend = "avx512")] {
                        let tokens = ();
                    } else if #[cfg(chacha20_backend = "avx2")] {
                        let tokens = ();
                    } else if #[cfg(chacha20_backend = "sse2")] {
                        let tokens = ();
                    } else if #[cfg(chacha20_avx512)] {
                        let tokens = (avx512_cpuid::init(), avx2_cpuid::init(), sse2_cpuid::init());
                    } else {
                        let tokens = (avx2_cpuid::init(), sse2_cpuid::init());
                    }
                }
            } else {
                let tokens = ();
            }
        }
        Self {
            state,
            tokens,
            _pd: PhantomData,
        }
    }

    /// Get the current block position.
    #[inline(always)]
    #[must_use]
    pub fn get_block_pos(&self) -> V::Counter {
        V::get_block_pos(&self.state[12..])
    }

    /// Set the block position.
    #[inline(always)]
    pub fn set_block_pos(&mut self, pos: V::Counter) {
        V::set_block_pos(&mut self.state[12..], pos);
    }
}

impl<R: Rounds, V: Variant> fmt::Debug for ChaChaCore<R, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ChaChaCore<R: {}, V: {}-bit)> {{ ... }}",
            R::COUNT,
            size_of::<V::Counter>() * 8
        )
    }
}

#[cfg(feature = "cipher")]
impl<R: Rounds, V: Variant> StreamCipherSeekCore for ChaChaCore<R, V> {
    type Counter = V::Counter;

    #[inline(always)]
    fn get_block_pos(&self) -> Self::Counter {
        self.get_block_pos()
    }

    #[inline(always)]
    fn set_block_pos(&mut self, pos: Self::Counter) {
        self.set_block_pos(pos);
    }
}

#[cfg(feature = "cipher")]
impl<R: Rounds, V: Variant> StreamCipherCore for ChaChaCore<R, V> {
    #[inline(always)]
    fn remaining_blocks(&self) -> Option<usize> {
        V::remaining_blocks(self.get_block_pos())
    }

    fn process_with_backend(
        &mut self,
        f: impl cipher::StreamCipherClosure<BlockSize = Self::BlockSize>,
    ) {
        cfg_if! {
            if #[cfg(chacha20_backend = "soft")] {
                f.call(&mut backends::soft::Backend(self));
            } else if #[cfg(any(target_arch = "x86", target_arch = "x86_64"))] {
                cfg_if! {
                    if #[cfg(all(chacha20_avx512, chacha20_backend = "avx512"))] {
                        unsafe {
                            backends::avx512::inner::<R, _, V>(&mut self.state, f);
                        }
                    } else if #[cfg(chacha20_backend = "avx2")] {
                        unsafe {
                            backends::avx2::inner::<R, _, V>(&mut self.state, f);
                        }
                    } else if #[cfg(chacha20_backend = "sse2")] {
                        unsafe {
                            backends::sse2::inner::<R, _, V>(&mut self.state, f);
                        }
                    } else {
                        #[cfg(chacha20_avx512)]
                        let (avx512_token, avx2_token, sse2_token) = self.tokens;
                        #[cfg(not(chacha20_avx512))]
                        let (avx2_token, sse2_token) = self.tokens;

                        #[cfg(chacha20_avx512)]
                        if avx512_token.get() {
                            // SAFETY: runtime CPU feature detection above ensures this is valid
                            unsafe {
                                backends::avx512::inner::<R, _, V>(&mut self.state, f);
                            }
                            return;
                        }
                        if avx2_token.get() {
                            // SAFETY: runtime CPU feature detection above ensures this is valid
                            unsafe {
                                backends::avx2::inner::<R, _, V>(&mut self.state, f);
                            }
                        } else if sse2_token.get() {
                            // SAFETY: runtime CPU feature detection above ensures this is valid
                            unsafe {
                                backends::sse2::inner::<R, _, V>(&mut self.state, f);
                            }
                        } else {
                            f.call(&mut backends::soft::Backend(self));
                        }
                    }
                }
            } else if #[cfg(all(target_arch = "aarch64", target_feature = "neon"))] {
                // SAFETY: we have used conditional compilation to ensure NEON is available
                unsafe {
                    backends::neon::inner::<R, _, V>(&mut self.state, f);
                }
            } else {
                f.call(&mut backends::soft::Backend(self));
            }
        }
    }
}

#[cfg(feature = "cipher")]
impl<R: Rounds, V: Variant> BlockSizeUser for ChaChaCore<R, V> {
    type BlockSize = U64;
}

#[cfg(feature = "zeroize")]
impl<R: Rounds, V: Variant> Drop for ChaChaCore<R, V> {
    fn drop(&mut self) {
        self.state.zeroize();
    }
}

#[cfg(feature = "zeroize")]
impl<R: Rounds, V: Variant> ZeroizeOnDrop for ChaChaCore<R, V> {}

/// The ChaCha20 quarter round function
///
/// We located this function in the root of the crate as we want it to be available
/// for the soft backend and for xchacha.
#[allow(dead_code)]
pub(crate) fn quarter_round(
    a: usize,
    b: usize,
    c: usize,
    d: usize,
    state: &mut [u32; STATE_WORDS],
) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(16);

    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(12);

    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(8);

    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(7);
}
