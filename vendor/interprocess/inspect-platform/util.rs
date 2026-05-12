#![cfg_attr(not(unix), allow(dead_code, unused_macros))]

use std::fmt::{self, Display, Formatter};

macro_rules! bitwidths {
    ($($nam:ident),+ $(,)?) => {[$((stringify!($nam), $nam::BITS, $nam::MIN == 0)),+]};
}
macro_rules! sizes {
    ($($nam:ident),+ $(,)?) => {[$((stringify!($nam), ::std::mem::size_of::<$nam>())),+]};
}
fn maxlen2<T>(a: &[(&str, T)]) -> usize { a.iter().map(|&(nm, _)| nm.len()).max().unwrap_or(0) }
fn maxlen3<T>(a: &[(&str, T, bool)]) -> usize {
    a.iter().map(|&(nm, _, _)| nm.len()).max().unwrap_or(0)
}

pub fn print_bitwidths(bw: &[(&str, u32, bool)]) {
    let width = maxlen3(bw);
    bw.iter().for_each(|&(nm, bw, un)| {
        println!("{nm:width$} : {bw:>2} bits {:>2}signed", val_if(un, "un"))
    });
}
pub fn print_sizes(sz: &[(&str, usize)]) {
    let width = maxlen2(sz);
    sz.iter().for_each(|&(nm, sz)| println!("{nm:width$} : {sz:>3} bytes"));
}

#[allow(clippy::obfuscated_if_else)]
pub fn val_if<T: Default>(cond: bool, v: T) -> T { cond.then_some(v).unwrap_or_default() }

pub const fn display_fn<T: Fn(&mut Formatter<'_>) -> fmt::Result>(v: T) -> DisplayFn<T> {
    DisplayFn(v)
}
#[derive(Copy, Clone, Debug, Default)]
pub struct DisplayFn<T>(T);
impl<T> From<T> for DisplayFn<T> {
    fn from(v: T) -> Self { Self(v) }
}
impl<T: Fn(&mut Formatter<'_>) -> fmt::Result> Display for DisplayFn<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { (self.0)(f) }
}

pub trait ResultExt: Sized {
    type Ok;
    type Err: Display;
    fn get_err(&self) -> Option<&Self::Err>;
    fn unwrap_or_else(self, f: impl FnOnce(Self::Err) -> Self::Ok) -> Self::Ok;

    fn report_error(self, msg: &str) -> Self {
        report_error_str(self.get_err(), msg);
        self
    }
    fn report_error_if(self, toggle: bool, msg: &str) -> Self {
        if toggle {
            report_error_str(self.get_err(), msg);
        }
        self
    }
    fn report_error_args(self, msg: fmt::Arguments<'_>) -> Self {
        report_error(self.get_err(), msg);
        self
    }
    fn report_error_args_if(self, toggle: bool, msg: fmt::Arguments<'_>) -> Self {
        if toggle {
            report_error(self.get_err(), msg);
        }
        self
    }
    fn set_if_error<T>(self, out: &mut T, val: T) -> Self {
        if self.get_err().is_some() {
            *out = val;
        }
        self
    }
    fn unwrap_or_exit(self, msg: &str) -> Self::Ok {
        report_error_str(self.get_err(), msg);
        self.unwrap_or_else(forget_error_and_die)
    }
    fn unwrap_or_exit_args(self, msg: fmt::Arguments<'_>) -> Self::Ok {
        report_error(self.get_err(), msg);
        self.unwrap_or_else(forget_error_and_die)
    }
}
#[inline(never)]
fn report_error<E: Display>(e: Option<&E>, msg: fmt::Arguments<'_>) {
    if let Some(e) = e {
        println!("{msg}: {e}");
    }
}
#[inline(never)]
fn report_error_str<E: Display>(e: Option<&E>, msg: &str) {
    if let Some(e) = e {
        println!("{msg}: {e}");
    }
}
fn forget_error_and_die<T, E>(e: E) -> T {
    std::mem::forget(e);
    std::process::exit(1);
}
impl<T, E: Display> ResultExt for Result<T, E> {
    type Ok = T;
    type Err = E;
    fn get_err(&self) -> Option<&E> { self.as_ref().err() }
    fn unwrap_or_else(self, f: impl FnOnce(E) -> T) -> T { self.unwrap_or_else(f) }
}
