use std::{
    collections::BTreeSet,
    fmt::Display,
    io::{self, Write},
};

use serde::{ser::Serializer as SerSerializer, Serialize};
use serde_json::{
    error::Result,
    ser::{CharEscape, Formatter, Serializer},
};

struct JsonProperty {
    sorting_key: Vec<u16>,
    key: Vec<u8>,
    value: Vec<u8>,
}

impl JsonProperty {
    fn new(key: Vec<u8>, value: Vec<u8>) -> io::Result<Self> {
        // Go through deserialization again to process escape sequences in the key
        // "\\a" should be processed as '\a' for sorting
        let sorting_key_as_value = serde_json::from_slice::<serde_json::Value>(&key)?;
        let sorting_key: Vec<u16> = sorting_key_as_value
            .as_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF8 sequence"))?
            .encode_utf16()
            .collect();
        Ok(Self {
            sorting_key,
            key,
            value,
        })
    }
}

impl PartialEq for JsonProperty {
    fn eq(&self, other: &Self) -> bool {
        self.sorting_key == other.sorting_key
    }
}

impl Eq for JsonProperty {}

impl PartialOrd for JsonProperty {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsonProperty {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sorting_key.cmp(&other.sorting_key)
    }
}

type JsonObject = BTreeSet<JsonProperty>;

/// The formatter that's used by the [JcsSerializer].
///
/// This formatter is not fully RFC 8785 compliant in its own right, because the [JcsSerializer] is
/// instead responsible for handling floating point NaN and infinity.
#[derive(Default)]
pub(crate) struct JcsFormatter {
    objects: Vec<JsonObject>,
    keys: Vec<Vec<u8>>,
    buffers: Vec<Vec<u8>>,
}

impl JcsFormatter {
    fn get_writer<'a, W>(&'a mut self, writer: &'a mut W) -> Box<dyn io::Write + 'a>
    where
        W: ?Sized + io::Write,
    {
        match self.buffers.last_mut() {
            Some(buffer) => Box::new(buffer),
            None => Box::new(writer),
        }
    }
}

impl Formatter for JcsFormatter {
    /// Writes a `null` value to the specified writer.
    fn write_null<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.get_writer(writer).write_all(b"null")
    }

    /// Writes a `true` or `false` value to the specified writer.
    fn write_bool<W>(&mut self, writer: &mut W, value: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let s = if value {
            b"true" as &[u8]
        } else {
            b"false" as &[u8]
        };
        self.get_writer(writer).write_all(s)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_i8<W>(&mut self, writer: &mut W, value: i8) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_i16<W>(&mut self, writer: &mut W, value: i16) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_i32<W>(&mut self, writer: &mut W, value: i32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_i64<W>(&mut self, writer: &mut W, value: i64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_i128<W>(&mut self, writer: &mut W, value: i128) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_u8<W>(&mut self, writer: &mut W, value: u8) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_u16<W>(&mut self, writer: &mut W, value: u16) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_u32<W>(&mut self, writer: &mut W, value: u32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_u64<W>(&mut self, writer: &mut W, value: u64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_u128<W>(&mut self, writer: &mut W, value: u128) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Javascript (and as a consequence JSON) only supports a single numeric
    /// type which is the double. Format all numbers according to this.
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.write_f64(writer, value as f64)
    }

    /// Writes a floating point value like `-31.26e+12` to the specified writer.
    /// JCS (and JSON in general) does not permit NaN or (-)Infinity
    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if value.is_finite() {
            let mut buffer = ryu_js::Buffer::new();
            let s = buffer.format_finite(value);
            self.get_writer(writer).write_all(s.as_bytes())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NaN and +/-Infinity are not permitted in JSON",
            ))
        }
    }

    /// Writes a number that has already been rendered to a string.
    /// To be JCS conformant the string is parsed into a double
    /// and reformatted.
    fn write_number_str<W>(&mut self, writer: &mut W, value: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let number: f64 = value
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Cannot parse str to f64"))?;
        self.write_f64(writer, number)
    }

    /// Called before each series of `write_string_fragment` and
    /// `write_char_escape`.  Writes a `"` to the specified writer.
    fn begin_string<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.get_writer(writer).write_all(b"\"")
    }

    /// Called after each series of `write_string_fragment` and
    /// `write_char_escape`.  Writes a `"` to the specified writer.
    fn end_string<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.get_writer(writer).write_all(b"\"")
    }

    /// Writes a string fragment that doesn't need any escaping to the
    /// specified writer.
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.get_writer(writer).write_all(fragment.as_bytes())
    }

    /// Writes a character escape code to the specified writer.
    fn write_char_escape<W>(&mut self, writer: &mut W, char_escape: CharEscape) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        use self::CharEscape::*;

        let s = match char_escape {
            Quote => b"\\\"",
            ReverseSolidus => b"\\\\",
            Solidus => {
                // This is according to the javascript reference implementation (https://www.npmjs.com/package/canonicalize) where
                // an escaped solidus is turned into a non escaped one, in javascript "\/" === "/".
                // RFC 8785 in Section 3.2.2.2 does not list a solidus as a special escape character.
                // Because of the return type of the match block we need to return here early
                return self.get_writer(writer).write_all(b"/");
            }
            Backspace => b"\\b",
            FormFeed => b"\\f",
            LineFeed => b"\\n",
            CarriageReturn => b"\\r",
            Tab => b"\\t",
            AsciiControl(byte) => {
                static HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";
                let bytes = &[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX_DIGITS[(byte >> 4) as usize],
                    HEX_DIGITS[(byte & 0xF) as usize],
                ];

                return self.get_writer(writer).write_all(bytes);
            }
        };

        self.get_writer(writer).write_all(s)
    }

    /// Called before every array.  Writes a `[` to the specified
    /// writer.
    #[inline]
    fn begin_array<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.get_writer(writer).write_all(b"[")
    }

    /// Called after every array.  Writes a `]` to the specified
    /// writer.
    #[inline]
    fn end_array<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.get_writer(writer).write_all(b"]")
    }

    /// Called before every array value.  Writes a `,` if needed to
    /// the specified writer.
    #[inline]
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            self.get_writer(writer).write_all(b",")
        }
    }

    /// Called after every array value.
    #[inline]
    fn end_array_value<W>(&mut self, _writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        Ok(())
    }

    /// Pushes a new empty object to the stack
    #[inline]
    fn begin_object<W>(&mut self, _writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.objects.push(Default::default());
        Ok(())
    }

    /// Writes out the whole object with sorted properties
    #[inline]
    fn end_object<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let json_object = self.objects.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "end_object called before start_object",
            )
        })?;
        let mut writer = self.get_writer(writer);
        writer.write_all(b"{")?;
        json_object
            .into_iter()
            .enumerate()
            .try_for_each(|(idx, property)| {
                if idx > 0 {
                    writer.write_all(b",")?;
                }
                writer.write_all(&property.key)?;
                writer.write_all(b":")?;
                writer.write_all(&property.value)
            })?;
        writer.write_all(b"}")
    }

    /// Creates a new buffer to direct writes into
    #[inline]
    fn begin_object_key<W>(&mut self, _writer: &mut W, _first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.buffers.push(Default::default());
        Ok(())
    }

    /// Moves the last buffer to the top of keys stack
    #[inline]
    fn end_object_key<W>(&mut self, _writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let key = self.buffers.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "end_object_key called before begin_object_key",
            )
        })?;
        self.keys.push(key);
        Ok(())
    }

    /// Creates a new buffer to direct writes into
    #[inline]
    fn begin_object_value<W>(&mut self, _writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.buffers.push(Vec::new());
        Ok(())
    }

    /// Creates a JsonProperty from the key-value last created and adds it to the top object on the stack
    #[inline]
    fn end_object_value<W>(&mut self, _writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let value = self.buffers.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "end_object_value called before begin_object_value",
            )
        })?;
        let key = self.keys.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "end_object_value called before end_object_key",
            )
        })?;
        let json_object = self.objects.last_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "end_object_value called before start_object",
            )
        })?;
        json_object.insert(JsonProperty::new(key, value)?);
        Ok(())
    }

    /// Raw fragments are not supported because it cannot be verified that they conform to JCS without modifying them
    #[inline]
    fn write_raw_fragment<W>(&mut self, _writer: &mut W, _fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Raw values are not supported for JCS serialization",
        ))
    }
}

/// An RFC 8785 compatible JSON Canonicalization Scheme (JCS) serializer for [serde_json].
pub(crate) struct JcsSerializer<W: io::Write> {
    serializer: Serializer<W, JcsFormatter>,
}

impl<W: io::Write> JcsSerializer<W> {
    /// Creates a new JSON serializer.
    #[inline]
    pub fn new(writer: W) -> Self {
        Self {
            serializer: Serializer::with_formatter(writer, JcsFormatter::default()),
        }
    }

    /// Consumes this serializer returning the underlying writer.
    #[inline]
    #[allow(dead_code)]
    pub fn into_inner(self) -> W {
        self.serializer.into_inner()
    }
}

impl<'a, W: io::Write> SerSerializer for &'a mut JcsSerializer<W> {
    type Ok = <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::Ok;
    type Error = <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::Error;

    type SerializeSeq = <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeSeq;
    type SerializeTuple = <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeTuple;
    type SerializeTupleStruct =
        <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeTupleStruct;
    type SerializeTupleVariant =
        <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeTupleVariant;
    type SerializeMap = <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeMap;
    type SerializeStruct = <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeStruct;
    type SerializeStructVariant =
        <&'a mut Serializer<W, JcsFormatter> as SerSerializer>::SerializeStructVariant;

    #[inline]
    fn serialize_bool(self, value: bool) -> Result<()> {
        self.serializer.serialize_bool(value)
    }

    #[inline]
    fn serialize_i8(self, value: i8) -> Result<()> {
        self.serializer.serialize_i8(value)
    }

    #[inline]
    fn serialize_i16(self, value: i16) -> Result<()> {
        self.serializer.serialize_i16(value)
    }

    #[inline]
    fn serialize_i32(self, value: i32) -> Result<()> {
        self.serializer.serialize_i32(value)
    }

    #[inline]
    fn serialize_i64(self, value: i64) -> Result<()> {
        self.serializer.serialize_i64(value)
    }

    fn serialize_i128(self, value: i128) -> Result<()> {
        self.serializer.serialize_i128(value)
    }

    #[inline]
    fn serialize_u8(self, value: u8) -> Result<()> {
        self.serializer.serialize_u8(value)
    }

    #[inline]
    fn serialize_u16(self, value: u16) -> Result<()> {
        self.serializer.serialize_u16(value)
    }

    #[inline]
    fn serialize_u32(self, value: u32) -> Result<()> {
        self.serializer.serialize_u32(value)
    }

    #[inline]
    fn serialize_u64(self, value: u64) -> Result<()> {
        self.serializer.serialize_u64(value)
    }

    fn serialize_u128(self, value: u128) -> Result<()> {
        self.serializer.serialize_u128(value)
    }

    #[inline]
    fn serialize_f32(self, value: f32) -> Result<()> {
        if value.is_finite() {
            self.serializer.serialize_f32(value)
        } else {
            Err(Self::Error::io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NaN and +/-Infinity are not permitted in JSON",
            )))
        }
    }

    #[inline]
    fn serialize_f64(self, value: f64) -> Result<()> {
        if value.is_finite() {
            self.serializer.serialize_f64(value)
        } else {
            Err(Self::Error::io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NaN and +/-Infinity are not permitted in JSON",
            )))
        }
    }

    #[inline]
    fn serialize_char(self, value: char) -> Result<()> {
        self.serializer.serialize_char(value)
    }

    #[inline]
    fn serialize_str(self, value: &str) -> Result<()> {
        self.serializer.serialize_str(value)
    }

    #[inline]
    fn serialize_bytes(self, value: &[u8]) -> Result<()> {
        self.serializer.serialize_bytes(value)
    }

    #[inline]
    fn serialize_unit(self) -> Result<()> {
        self.serializer.serialize_unit()
    }

    #[inline]
    fn serialize_unit_struct(self, name: &'static str) -> Result<()> {
        self.serializer.serialize_unit_struct(name)
    }

    #[inline]
    fn serialize_unit_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<()> {
        self.serializer
            .serialize_unit_variant(name, variant_index, variant)
    }

    /// Serialize newtypes without an object wrapper.
    #[inline]
    fn serialize_newtype_struct<T>(self, name: &'static str, value: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        self.serializer.serialize_newtype_struct(name, value)
    }

    #[inline]
    fn serialize_newtype_variant<T>(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        self.serializer
            .serialize_newtype_variant(name, variant_index, variant, value)
    }

    #[inline]
    fn serialize_none(self) -> Result<()> {
        self.serializer.serialize_none()
    }

    #[inline]
    fn serialize_some<T>(self, value: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        self.serializer.serialize_some(value)
    }

    #[inline]
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
        self.serializer.serialize_seq(len)
    }

    #[inline]
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple> {
        self.serializer.serialize_tuple(len)
    }

    #[inline]
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        self.serializer.serialize_tuple_struct(name, len)
    }

    #[inline]
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        self.serializer
            .serialize_tuple_variant(name, variant_index, variant, len)
    }

    #[inline]
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap> {
        self.serializer.serialize_map(len)
    }

    #[inline]
    fn serialize_struct(self, name: &'static str, len: usize) -> Result<Self::SerializeStruct> {
        self.serializer.serialize_struct(name, len)
    }

    #[inline]
    fn serialize_struct_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        self.serializer
            .serialize_struct_variant(name, variant_index, variant, len)
    }

    fn collect_str<T>(self, value: &T) -> Result<()>
    where
        T: ?Sized + Display,
    {
        self.serializer.collect_str(value)
    }
}
