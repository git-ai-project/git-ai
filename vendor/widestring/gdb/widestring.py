import gdb
import traceback
import gdb.printing
import unicodedata

def codepoints_to_rust_string(data_ptr, length: int) -> str:
    def to_rust_string(cp: int) -> str:
        c = chr(cp)
        # Surrogate or private use.
        if unicodedata.category(c) in ('Cs', 'Co'):
            return f"\\u{{{cp:04x}}}" if cp <= 0xffff else f"\\U{{{cp:08x}}}"
        if c in '\\"':
            return "\\" + c
        return c
    codepoints = (int((data_ptr + i).dereference()) for i in range(length))
    return '"' + ''.join(to_rust_string(cp) for cp in codepoints) + '"'

class Utf32StrPrinter:
    def __init__(self, val):
        self.val = val

    def to_string(self):
        try:
            ptr_to_slice = self.val.address.cast(gdb.lookup_type("usize").pointer())
            length = int(ptr_to_slice[1])
            ptr = ptr_to_slice[0].cast(gdb.lookup_type("u32").pointer())
            return codepoints_to_rust_string(ptr, length)
        except Exception:
            return "error reading Utf32Str:\n" + traceback.format_exc()

class Utf32StringPrinter:
    def __init__(self, val):
        self.val = val

    def to_string(self):
        try:
            inner = self.val['inner']
            ptr = inner['buf']['inner']['ptr']['pointer']['pointer'].cast(gdb.lookup_type("u32").pointer())
            len = int(inner['len'])
            return codepoints_to_rust_string(ptr, len)
        except Exception:
            return "error reading Utf32String:\n" + traceback.format_exc()

def widestring_pretty_printer():
    pp = gdb.printing.RegexpCollectionPrettyPrinter("widestring-rs")
    pp.add_printer('Utf32Str', '^&widestring::utfstr::Utf32Str$', Utf32StrPrinter)
    pp.add_printer('Utf32String', '^widestring::utfstring::Utf32String$', Utf32StringPrinter)
    return pp
