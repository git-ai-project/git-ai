#![allow(clippy::exit, clippy::incompatible_msrv)]

#[macro_use]
mod util;

use {
    std::ffi::{c_char, c_int, c_long, c_longlong, c_short},
    util::*,
};

#[cfg(unix)]
mod libc_wrappers;
#[cfg(unix)]
mod unix;

fn print_common_intro() {
    use std::env::consts::*;
    println!("==== interprocess inspect-platform on {} {} ====", OS, ARCH);
    print_bitwidths(&bitwidths!(usize, c_char, c_short, c_int, c_long, c_longlong));
}

fn main() {
    print_common_intro();
    #[cfg(unix)]
    unix::main();
    #[cfg(not(unix))]
    println!("Not a Unix system, no further information will be gathered.");
}
