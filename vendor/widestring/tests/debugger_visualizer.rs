#![allow(unsupported_calling_conventions)]
#![cfg(not(windows))]

use debugger_test::debugger_test;
use widestring::*;

#[inline(never)]
fn __break() {}

#[debugger_test(
    debugger = "cdb",
    commands = r#"
.nvlist
dx u16_string

dx u32_string

dx u16_cstring

dx u32_cstring

dx utf16_string

dx utf32_string

dx u16_cstr

dx u32_cstr
"#,
    expected_statements = r#"
u16_string       : "my u16 string" [Type: widestring::ustring::U16String]
[<Raw View>]     [Type: widestring::ustring::U16String]
[len]            : 0xd [Type: unsigned __int64]
[chars]

u32_string       : "my u32 string" [Type: widestring::ustring::U32String]
[<Raw View>]     [Type: widestring::ustring::U32String]
[len]            : 0xd [Type: unsigned __int64]
[chars]

u16_cstring      : "my u16 cstring" [Type: widestring::ucstring::U16CString]
[<Raw View>]     [Type: widestring::ucstring::U16CString]
[len]            : 0xf [Type: unsigned __int64]
[chars]

u32_cstring      : "my u32 cstring" [Type: widestring::ucstring::U32CString]
[<Raw View>]     [Type: widestring::ucstring::U32CString]
[len]            : 0xf [Type: unsigned __int64]
[chars]

utf16_string     : "my utf16 string" [Type: widestring::utfstring::Utf16String]
[<Raw View>]     [Type: widestring::utfstring::Utf16String]
[len]            : 0xf [Type: unsigned __int64]
[chars]

utf32_string     : "my utf32 string" [Type: widestring::utfstring::Utf32String]
[<Raw View>]     [Type: widestring::utfstring::Utf32String]
[len]            : 0xf [Type: unsigned __int64]
[chars]

u16_cstr         [Type: ref$<widestring::ucstr::U16CStr>]
pattern:\[\+0x000\] data_ptr         : 0x[0-9a-f]+ : "my u16 cstr" \[Type: widestring::ucstr::U16CStr \*\]
[+0x008] length           : 0xc [Type: unsigned __int64]

u32_cstr         [Type: ref$<widestring::ucstr::U32CStr>]
pattern:\[\+0x000\] data_ptr         : 0x[0-9a-f]+ : "my u32 cstr" \[Type: widestring::ucstr::U32CStr \*\]
[+0x008] length           : 0xc [Type: unsigned __int64]
"#
)]
fn test_debugger_visualizer() {
    let u16_string = U16String::from_str("my u16 string");
    assert!(!u16_string.is_empty());

    let u32_string = U32String::from_str("my u32 string");
    assert!(!u32_string.is_empty());

    let u16_cstring = U16CString::from_str("my u16 cstring").unwrap();
    assert!(!u16_cstring.is_empty());

    let u32_cstring = U32CString::from_str("my u32 cstring").unwrap();
    assert!(!u32_cstring.is_empty());

    let utf16_string = Utf16String::from_str("my utf16 string");
    assert!(!utf16_string.is_empty());

    let utf32_string = Utf32String::from_str("my utf32 string");
    assert!(!utf32_string.is_empty());

    let u16_cstr = u16cstr!("my u16 cstr");
    assert!(!u16_cstr.is_empty());

    let u32_cstr = u32cstr!("my u32 cstr");
    assert!(!u32_cstr.is_empty());
    __break(); // #break
}
