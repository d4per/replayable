//! The binary log format, written by [`Store::write_binary`] and read by
//! [`Store::read_binary`]. The layout is described in the README, under
//! "Binary layout", which is also the crate's documentation.
//!
//! [`Store::write_binary`]: crate::Store::write_binary
//! [`Store::read_binary`]: crate::Store::read_binary

use crate::codec::{self, Parsed};
use crate::time::MAX_MILLIS;
use crate::{Error, Model, Timestamp};
use serde::de::{self, DeserializeSeed, Visitor};
use serde::ser::{self, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};
use std::io;

/// The start of every binary log: "RPLB" and the format version.
const HEADER: &[u8; 5] = b"RPLB\x01";

const UNIT: u8 = 0x00;
const FALSE: u8 = 0x01;
const TRUE: u8 = 0x02;
const SOME: u8 = 0x03;
const UINT: u8 = 0x04;
const NEG: u8 = 0x05;
const F32: u8 = 0x06;
const F64: u8 = 0x07;
const STR: u8 = 0x08;
const BYTES: u8 = 0x09;
const SEQ: u8 = 0x0A;
const MAP: u8 = 0x0B;
const NAME_DEF: u8 = 0x0C;
const NAME_REF: u8 = 0x0D;
const SEQ_OPEN: u8 = 0x0E;
const MAP_OPEN: u8 = 0x0F;
const END: u8 = 0x10;
const NAME_SMALL: u8 = 0x20;
const SEQ_SMALL: u8 = 0x50;
const MAP_SMALL: u8 = 0x60;
const STR_SMALL: u8 = 0x70;
const UINT_SMALL: u8 = 0x80;

/// Names 0–47 fit in the tag byte.
const SMALL_NAMES: u32 = 48;
/// Longer map keys are written as plain strings.
const MAX_NAME_LEN: usize = 64;
/// Once a log has this many names, new ones are written as plain strings.
/// Plenty for the names in a model, and small enough that keys picked to
/// collide in the hash table can't slow writing down much.
const MAX_NAMES: usize = 1 << 12;
/// How deeply values may be nested when reading, so that a corrupt log
/// can't overflow the stack.
const MAX_DEPTH: u32 = 128;

/// Why a value couldn't be written or read. Boxed, so that results stay
/// small: they're returned from every call while reading.
#[derive(Debug)]
#[allow(clippy::box_collection)]
pub(crate) struct BinaryError(Box<String>);

impl BinaryError {
    fn new(message: impl Into<String>) -> Self {
        BinaryError(Box::new(message.into()))
    }
}

impl fmt::Display for BinaryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BinaryError {}

impl ser::Error for BinaryError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        BinaryError::new(msg.to_string())
    }
}

impl de::Error for BinaryError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        BinaryError::new(msg.to_string())
    }
}

type Result<T> = std::result::Result<T, BinaryError>;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_varint128(out: &mut Vec<u8>, mut v: u128) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

// ---------------------------------------------------------------------------
// Writing

/// FNV-1a: names are short and looked up several times per event, and this
/// is about a quarter faster than the default hasher. Map keys can come from
/// outside, so someone could pick keys that collide, but the table is kept
/// small (`MAX_NAMES`), which bounds the damage.
#[derive(Default)]
struct Fnv(u64);

impl Hasher for Fnv {
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf2_9ce4_8422_2325 } else { self.0 };
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type Names = HashMap<String, u32, BuildHasherDefault<Fnv>>;

/// What a store knows about the binary file it last wrote to or read from,
/// so that it can append to it: the names defined in it, and the time of
/// its last event.
#[derive(Default)]
pub(crate) struct WriteState {
    names: Names,
    last: Timestamp,
    /// How many of the store's events the file holds. `None` if unknown.
    pub(crate) upto: Option<usize>,
    /// Whether the file starts with the header.
    pub(crate) has_header: bool,
}

impl WriteState {
    fn reset(&mut self) {
        self.names.clear();
        self.last = Timestamp::UNIX_EPOCH;
    }
}

/// Writes `events`, which follow the first `n` events of the log, for a file
/// that holds those `n`. With `header`, it's a new file, which gets the
/// header first. Otherwise, if `state` isn't about a file with `n` events, a
/// reset record comes first, so the result is right either way.
pub(crate) fn write<M: Model>(
    state: &mut WriteState,
    n: usize,
    header: bool,
    times: &[Timestamp],
    events: &[M::Event],
    mut writer: impl io::Write,
) -> io::Result<()> {
    if times.is_empty() {
        return Ok(());
    }
    let mut out = Vec::with_capacity(times.len().min(1 << 12) * 32);
    if header {
        out.extend_from_slice(HEADER);
        state.reset();
    } else if state.upto != Some(n) {
        out.push(0);
        state.reset();
    }
    // Unknown until everything is written: if something fails, the next
    // write starts with a reset.
    state.upto = None;
    for (&at, event) in times.iter().zip(events) {
        if at < state.last {
            out.push(0);
            state.reset();
        }
        put_varint(&mut out, at.unix_millis() - state.last.unix_millis() + 1);
        state.last = at;
        let mut w = Writer { out: &mut out, names: &mut state.names, key: false };
        codec::serialize_event::<M, _>(event, &mut w).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if out.len() >= 1 << 16 {
            writer.write_all(&out)?;
            out.clear();
        }
    }
    writer.write_all(&out)?;
    state.upto = Some(n + times.len());
    state.has_header = true;
    Ok(())
}

/// A serde serializer into the binary format.
struct Writer<'a> {
    out: &'a mut Vec<u8>,
    names: &'a mut Names,
    /// Set while a map key is written, so that a string key becomes a name.
    key: bool,
}

impl Writer<'_> {
    fn uint(&mut self, v: u64) {
        if v < 0x80 {
            self.out.push(UINT_SMALL | v as u8);
        } else {
            self.out.push(UINT);
            put_varint(self.out, v);
        }
    }

    fn int(&mut self, v: i64) {
        if v >= 0 {
            self.uint(v as u64);
        } else {
            self.out.push(NEG);
            put_varint(self.out, !v as u64);
        }
    }

    fn header(&mut self, small: u8, counted: u8, open: u8, len: Option<usize>) {
        match len {
            Some(n) if n < 16 => self.out.push(small | n as u8),
            Some(n) => {
                self.out.push(counted);
                put_varint(self.out, n as u64);
            }
            None => self.out.push(open),
        }
    }

    fn str(&mut self, s: &str) {
        if s.len() < 16 {
            self.out.push(STR_SMALL | s.len() as u8);
        } else {
            self.out.push(STR);
            put_varint(self.out, s.len() as u64);
        }
        self.out.extend_from_slice(s.as_bytes());
    }

    fn name(&mut self, s: &str) {
        if let Some(&i) = self.names.get(s) {
            if i < SMALL_NAMES {
                self.out.push(NAME_SMALL + i as u8);
            } else {
                self.out.push(NAME_REF);
                put_varint(self.out, u64::from(i));
            }
        } else if s.len() <= MAX_NAME_LEN && self.names.len() < MAX_NAMES {
            self.names.insert(s.to_owned(), self.names.len() as u32);
            self.out.push(NAME_DEF);
            put_varint(self.out, s.len() as u64);
            self.out.extend_from_slice(s.as_bytes());
        } else {
            self.str(s);
        }
    }

    /// The start of an enum variant with content: a map with one entry,
    /// from the variant's name.
    fn variant(&mut self, variant: &str) {
        self.out.push(MAP_SMALL | 1);
        self.name(variant);
    }
}

/// A sequence or map being written. Checks that the number of items given
/// up front was right.
struct Compound<'a, 'b> {
    w: &'a mut Writer<'b>,
    len: Option<usize>,
    count: usize,
}

impl Compound<'_, '_> {
    fn finish(self) -> Result<()> {
        match self.len {
            None => {
                self.w.out.push(END);
                Ok(())
            }
            Some(n) if n == self.count => Ok(()),
            Some(n) => Err(BinaryError::new(format!("a value said it had {n} items, but had {}", self.count))),
        }
    }
}

impl<'a, 'b> ser::Serializer for &'a mut Writer<'b> {
    type Ok = ();
    type Error = BinaryError;
    type SerializeSeq = Compound<'a, 'b>;
    type SerializeTuple = Compound<'a, 'b>;
    type SerializeTupleStruct = Compound<'a, 'b>;
    type SerializeTupleVariant = Compound<'a, 'b>;
    type SerializeMap = Compound<'a, 'b>;
    type SerializeStruct = Compound<'a, 'b>;
    type SerializeStructVariant = Compound<'a, 'b>;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.out.push(if v { TRUE } else { FALSE });
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<()> {
        self.serialize_i64(v.into())
    }
    fn serialize_i16(self, v: i16) -> Result<()> {
        self.serialize_i64(v.into())
    }
    fn serialize_i32(self, v: i32) -> Result<()> {
        self.serialize_i64(v.into())
    }
    fn serialize_i64(self, v: i64) -> Result<()> {
        self.int(v);
        Ok(())
    }
    fn serialize_i128(self, v: i128) -> Result<()> {
        if let Ok(v) = i64::try_from(v) {
            self.int(v);
        } else if v >= 0 {
            self.out.push(UINT);
            put_varint128(self.out, v as u128);
        } else {
            self.out.push(NEG);
            put_varint128(self.out, !v as u128);
        }
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<()> {
        self.serialize_u64(v.into())
    }
    fn serialize_u16(self, v: u16) -> Result<()> {
        self.serialize_u64(v.into())
    }
    fn serialize_u32(self, v: u32) -> Result<()> {
        self.serialize_u64(v.into())
    }
    fn serialize_u64(self, v: u64) -> Result<()> {
        self.uint(v);
        Ok(())
    }
    fn serialize_u128(self, v: u128) -> Result<()> {
        if let Ok(v) = u64::try_from(v) {
            self.uint(v);
        } else {
            self.out.push(UINT);
            put_varint128(self.out, v);
        }
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<()> {
        self.out.push(F32);
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result<()> {
        self.out.push(F64);
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<()> {
        self.serialize_str(v.encode_utf8(&mut [0; 4]))
    }
    fn serialize_str(self, v: &str) -> Result<()> {
        if std::mem::take(&mut self.key) {
            self.name(v);
        } else {
            self.str(v);
        }
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        self.out.push(BYTES);
        put_varint(self.out, v.len() as u64);
        self.out.extend_from_slice(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<()> {
        self.out.push(UNIT);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<()> {
        self.out.push(SOME);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<()> {
        self.out.push(UNIT);
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(self, _name: &'static str, _index: u32, variant: &'static str) -> Result<()> {
        self.name(variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<()> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<()> {
        self.variant(variant);
        value.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Compound<'a, 'b>> {
        self.header(SEQ_SMALL, SEQ, SEQ_OPEN, len);
        Ok(Compound { w: self, len, count: 0 })
    }
    fn serialize_tuple(self, len: usize) -> Result<Compound<'a, 'b>> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Compound<'a, 'b>> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Compound<'a, 'b>> {
        self.variant(variant);
        self.serialize_seq(Some(len))
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Compound<'a, 'b>> {
        self.header(MAP_SMALL, MAP, MAP_OPEN, len);
        Ok(Compound { w: self, len, count: 0 })
    }
    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<Compound<'a, 'b>> {
        self.serialize_map(Some(len))
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Compound<'a, 'b>> {
        self.variant(variant);
        self.serialize_map(Some(len))
    }
}

impl Compound<'_, '_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.count += 1;
        value.serialize(&mut *self.w)
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<()> {
        self.count += 1;
        self.w.name(key);
        value.serialize(&mut *self.w)
    }
}

impl ser::SerializeSeq for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.element(value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.element(value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.element(value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.element(value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeMap for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<()> {
        self.count += 1;
        self.w.key = true;
        let result = key.serialize(&mut *self.w);
        self.w.key = false;
        result
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut *self.w)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<()> {
        self.field(key, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_, '_> {
    type Ok = ();
    type Error = BinaryError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<()> {
        self.field(key, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

// ---------------------------------------------------------------------------
// Reading

/// Where a binary log is read from.
pub(crate) enum Input<'a> {
    /// All of it, in memory.
    Bytes(&'a [u8]),
    /// A stream, read in chunks.
    Stream(&'a mut dyn io::Read),
}

/// How much of a stream is read at a time. The buffer grows if one event is
/// larger than this.
const CHUNK: usize = 16 << 10;

/// How reading a binary log ended.
pub(crate) struct ReadEnd {
    /// What's needed to append to the log. `upto` is left for the caller.
    pub(crate) state: WriteState,
    /// The offset just after the last whole record (or the header).
    pub(crate) complete: u64,
    /// Whether the log ends in the middle of a record or of the header.
    pub(crate) cut_off: bool,
}

/// Reads a binary log, calling `apply` for every event.
///
/// A stream is read a chunk at a time, and each chunk is parsed in place.
/// An event that's only partly in the buffer is parsed again once more has
/// been read. With `allow_cut_off`, a log that ends in the middle of an
/// event isn't an error: reading stops after the last whole one.
pub(crate) fn read<M: Model>(
    input: Input<'_>,
    allow_cut_off: bool,
    apply: impl FnMut(Timestamp, Parsed<M::Event>) -> std::result::Result<(), Error>,
) -> std::result::Result<ReadEnd, Error> {
    read_chunked::<M>(input, CHUNK, allow_cut_off, apply)
}

/// [`read`], with the size of a chunk given, for tests.
fn read_chunked<M: Model>(
    input: Input<'_>,
    chunk: usize,
    allow_cut_off: bool,
    mut apply: impl FnMut(Timestamp, Parsed<M::Event>) -> std::result::Result<(), Error>,
) -> std::result::Result<ReadEnd, Error> {
    let bad = |offset, reason: String| Error::Binary { offset, reason };
    let (bytes, mut stream, mut eof) = match input {
        Input::Bytes(bytes) => (bytes, None, true),
        Input::Stream(stream) => (&[][..], Some(stream), false),
    };
    let mut buf = Vec::new();
    if let Some(stream) = &mut stream {
        buf.reserve(chunk);
        eof = fill(&mut buf, &mut **stream, HEADER.len())?;
    }
    let data = if stream.is_some() { &buf[..] } else { bytes };
    let mut end = ReadEnd { state: WriteState::default(), complete: 0, cut_off: false };
    if data.is_empty() {
        return Ok(end);
    }
    if data.len() < HEADER.len() && allow_cut_off && HEADER.starts_with(data) {
        end.cut_off = true;
        return Ok(end);
    }
    if data.len() < HEADER.len() || data[..4] != HEADER[..4] {
        return Err(bad(0, "not a replayable binary log: it doesn't start with `RPLB`".into()));
    }
    if data[4] != HEADER[4] {
        return Err(bad(
            4,
            format!("the log is in version {} of the binary format, but only version 1 can be read", data[4]),
        ));
    }
    // The names defined in earlier chunks, and where the buffer starts in
    // the log.
    let mut names: Vec<Box<str>> = Vec::new();
    let mut offset = 0u64;
    let mut pos = HEADER.len();
    let mut last = Timestamp::UNIX_EPOCH;
    loop {
        let data = if stream.is_some() { &buf[..] } else { bytes };
        let mut r = Reader::new(data, &names);
        r.pos = pos;
        let stopped = loop {
            if r.pos == data.len() {
                break None;
            }
            let (start, defined) = (r.pos, r.new.len());
            r.ran_out = false;
            match record::<M>(&mut r, &mut last) {
                Ok(Some((at, parsed))) => apply(at, parsed)?,
                Ok(None) => {}
                Err(e) => {
                    // Parse it again once there's more, or stop before it.
                    r.pos = start;
                    r.new.truncate(defined);
                    if r.ran_out && (!eof || allow_cut_off) {
                        break None;
                    }
                    break Some((start, e));
                }
            }
        };
        pos = r.pos;
        let (new, cleared): (Vec<Box<str>>, _) = (r.new.iter().map(|&name| name.into()).collect(), r.cleared);
        if cleared {
            names.clear();
        }
        names.extend(new);
        if let Some((start, e)) = stopped {
            return Err(bad(offset + start as u64, *e.0));
        }
        let stream = match &mut stream {
            Some(stream) if !eof => stream,
            _ => {
                end.complete = offset + pos as u64;
                end.cut_off = pos < data.len();
                break;
            }
        };
        // Keep the part of the buffer that hasn't been parsed, and read more.
        buf.drain(..pos);
        offset += pos as u64;
        pos = 0;
        if buf.len() == buf.capacity() {
            buf.reserve(buf.len().max(chunk));
        }
        let want = buf.capacity();
        eof = fill(&mut buf, &mut **stream, want)?;
    }
    end.state.names = names.into_iter().enumerate().map(|(i, name)| (name.into(), i as u32)).collect();
    end.state.last = last;
    end.state.has_header = true;
    Ok(end)
}

/// Reads one record: an event, or `None` for a reset.
fn record<M: Model>(r: &mut Reader<'_>, last: &mut Timestamp) -> Result<Option<(Timestamp, Parsed<M::Event>)>> {
    let d = r.varint()?;
    if d == 0 {
        r.reset();
        *last = Timestamp::UNIX_EPOCH;
        return Ok(None);
    }
    let millis = last.unix_millis().saturating_add(d - 1);
    if millis > MAX_MILLIS {
        return Err(BinaryError::new("the time of the event is after year 9999"));
    }
    let parsed = codec::parse_event::<M, _>(&mut *r)?;
    *last = Timestamp::from_unix_millis(millis);
    Ok(Some((*last, parsed)))
}

/// Reads from `stream` into the spare capacity of `buf`, until `buf` holds
/// `want` bytes or the stream ends. Returns whether it ended.
fn fill(buf: &mut Vec<u8>, stream: &mut dyn io::Read, want: usize) -> io::Result<bool> {
    let mut len = buf.len();
    buf.resize(buf.capacity().max(want), 0);
    let result = loop {
        if len >= want {
            break Ok(false);
        }
        match stream.read(&mut buf[len..]) {
            Ok(0) => break Ok(true),
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    buf.truncate(len);
    result
}

/// A serde deserializer over a binary log in memory, or the part of one in
/// the buffer, so that names and strings can be borrowed from it.
struct Reader<'de> {
    input: &'de [u8],
    pos: usize,
    /// The names defined before `input`, in order.
    old: &'de [Box<str>],
    /// The names defined in `input`, in order, after `old`.
    new: Vec<&'de str>,
    /// Whether a reset record cleared `old`.
    cleared: bool,
    depth: u32,
    /// Set when the input ran out, so that the event can be read again once
    /// there's more.
    ran_out: bool,
}

fn cut_off() -> BinaryError {
    BinaryError::new("the log ends in the middle of an event; it may have been cut off while being written")
}

impl<'de> Reader<'de> {
    fn new(input: &'de [u8], old: &'de [Box<str>]) -> Self {
        Reader { input, pos: 0, old, new: Vec::new(), cleared: false, depth: 0, ran_out: false }
    }

    fn short(&mut self) -> BinaryError {
        self.ran_out = true;
        cut_off()
    }

    fn byte(&mut self) -> Result<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    fn peek(&mut self) -> Result<u8> {
        match self.input.get(self.pos) {
            Some(&b) => Ok(b),
            None => Err(self.short()),
        }
    }

    fn take(&mut self, n: usize) -> Result<&'de [u8]> {
        let Some(end) = self.pos.checked_add(n).filter(|&end| end <= self.input.len()) else {
            return Err(self.short());
        };
        let bytes = &self.input[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                break;
            }
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(BinaryError::new("a number is too large"))
    }

    fn varint128(&mut self) -> Result<u128> {
        let mut v = 0;
        for shift in (0..128).step_by(7) {
            let b = self.byte()?;
            if shift == 126 && b > 3 {
                break;
            }
            v |= u128::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(BinaryError::new("a number is too large"))
    }

    fn len(&mut self) -> Result<usize> {
        usize::try_from(self.varint()?).map_err(|_| BinaryError::new("a length is too large"))
    }

    fn str(&mut self, n: usize) -> Result<&'de str> {
        std::str::from_utf8(self.take(n)?).map_err(|_| BinaryError::new("a string isn't valid UTF-8"))
    }

    fn define(&mut self) -> Result<&'de str> {
        let n = self.len()?;
        let name = self.str(n)?;
        self.new.push(name);
        Ok(name)
    }

    fn name(&self, i: usize) -> Result<&'de str> {
        let name = match i.checked_sub(self.old.len()) {
            None => Some(&*self.old[i]),
            Some(j) => self.new.get(j).copied(),
        };
        name.ok_or_else(|| BinaryError::new(format!("name #{i} is used before it's defined")))
    }

    /// A reset record: forgets all names.
    fn reset(&mut self) {
        self.old = &[];
        self.new.clear();
        self.cleared = true;
    }

    /// Reads a string in any of its forms, after its tag.
    fn string(&mut self, tag: u8) -> Result<Option<&'de str>> {
        Ok(Some(match tag {
            STR => {
                let n = self.len()?;
                self.str(n)?
            }
            STR_SMALL..=0x7F => self.str(usize::from(tag & 0x0F))?,
            NAME_DEF => self.define()?,
            NAME_REF => {
                let i = self.len()?;
                self.name(i)?
            }
            NAME_SMALL..=0x4F => self.name(usize::from(tag - NAME_SMALL))?,
            _ => return Ok(None),
        }))
    }

    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth >= MAX_DEPTH {
            return Err(BinaryError::new(format!("values are nested more than {MAX_DEPTH} levels deep")));
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn seq<V: Visitor<'de>>(&mut self, len: Option<usize>, visitor: V) -> Result<V::Value> {
        self.nested(|r| {
            let mut items = Items { r, left: len, done: false };
            let value = visitor.visit_seq(&mut items)?;
            items.finish()?;
            Ok(value)
        })
    }

    fn map<V: Visitor<'de>>(&mut self, len: Option<usize>, visitor: V) -> Result<V::Value> {
        self.nested(|r| {
            let mut items = Items { r, left: len, done: false };
            let value = visitor.visit_map(&mut items)?;
            items.finish()?;
            Ok(value)
        })
    }

    /// Skips a value, still taking note of the names it defines.
    fn skip(&mut self) -> Result<()> {
        let tag = self.byte()?;
        if self.string(tag)?.is_some() {
            return Ok(());
        }
        let items = |r: &mut Self, n: usize| r.nested(|r| (0..n).try_for_each(|_| r.skip()));
        let until_end = |r: &mut Self| {
            r.nested(|r| {
                while r.peek()? != END {
                    r.skip()?;
                }
                r.pos += 1;
                Ok(())
            })
        };
        match tag {
            UNIT | FALSE | TRUE | UINT_SMALL..=0xFF => Ok(()),
            SOME => self.nested(Self::skip),
            UINT | NEG => self.varint128().map(drop),
            F32 => self.take(4).map(drop),
            F64 => self.take(8).map(drop),
            BYTES => {
                let n = self.len()?;
                self.take(n).map(drop)
            }
            SEQ => {
                let n = self.len()?;
                items(self, n)
            }
            MAP => {
                let n = self.len()?;
                items(self, n)?;
                items(self, n)
            }
            SEQ_SMALL..=0x5F => items(self, usize::from(tag & 0x0F)),
            MAP_SMALL..=0x6F => items(self, 2 * usize::from(tag & 0x0F)),
            SEQ_OPEN | MAP_OPEN => until_end(self),
            _ => Err(unknown_tag(tag)),
        }
    }
}

fn unknown_tag(tag: u8) -> BinaryError {
    BinaryError::new(format!("unknown tag 0x{tag:02X}"))
}

/// The items of a sequence or map: `left` of them, or up to `END`.
struct Items<'a, 'de> {
    r: &'a mut Reader<'de>,
    left: Option<usize>,
    done: bool,
}

impl Items<'_, '_> {
    fn next(&mut self) -> Result<bool> {
        match &mut self.left {
            Some(0) => Ok(false),
            Some(n) => {
                *n -= 1;
                Ok(true)
            }
            None if self.done => Ok(false),
            None if self.r.peek()? == END => {
                self.r.pos += 1;
                self.done = true;
                Ok(false)
            }
            None => Ok(true),
        }
    }

    /// Fails if the visitor stopped before the end.
    fn finish(mut self) -> Result<()> {
        match self.next()? {
            false => Ok(()),
            true => Err(BinaryError::new("more items than expected")),
        }
    }
}

impl<'de> de::SeqAccess<'de> for Items<'_, 'de> {
    type Error = BinaryError;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        if self.next()? {
            seed.deserialize(&mut *self.r).map(Some)
        } else {
            Ok(None)
        }
    }
    fn size_hint(&self) -> Option<usize> {
        self.left
    }
}

impl<'de> de::MapAccess<'de> for Items<'_, 'de> {
    type Error = BinaryError;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.next()? {
            seed.deserialize(&mut *self.r).map(Some)
        } else {
            Ok(None)
        }
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(&mut *self.r)
    }
    fn size_hint(&self) -> Option<usize> {
        self.left
    }
}

/// An enum variant with content, after the map tag.
struct Variant<'a, 'de> {
    r: &'a mut Reader<'de>,
}

impl<'de> de::EnumAccess<'de> for Variant<'_, 'de> {
    type Error = BinaryError;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let value = seed.deserialize(&mut *self.r)?;
        Ok((value, self))
    }
}

impl<'de> de::VariantAccess<'de> for Variant<'_, 'de> {
    type Error = BinaryError;
    fn unit_variant(self) -> Result<()> {
        de::Deserialize::deserialize(self.r)
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(self.r)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_any(self.r, visitor)
    }
    fn struct_variant<V: Visitor<'de>>(self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_any(self.r, visitor)
    }
}

impl<'de> de::Deserializer<'de> for &mut Reader<'de> {
    type Error = BinaryError;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let tag = self.byte()?;
        if let Some(s) = self.string(tag)? {
            return visitor.visit_borrowed_str(s);
        }
        match tag {
            UNIT => visitor.visit_unit(),
            FALSE => visitor.visit_bool(false),
            TRUE => visitor.visit_bool(true),
            SOME => self.nested(|r| visitor.visit_some(r)),
            UINT => {
                let start = self.pos;
                match self.varint() {
                    Ok(v) => visitor.visit_u64(v),
                    // Larger than a u64.
                    Err(_) => {
                        self.pos = start;
                        visitor.visit_u128(self.varint128()?)
                    }
                }
            }
            NEG => {
                let v = i128::try_from(self.varint128()?).map_err(|_| BinaryError::new("a number is too large"))?;
                match i64::try_from(!v) {
                    Ok(n) => visitor.visit_i64(n),
                    Err(_) => visitor.visit_i128(!v),
                }
            }
            F32 => visitor.visit_f32(f32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes"))),
            F64 => visitor.visit_f64(f64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes"))),
            BYTES => {
                let n = self.len()?;
                visitor.visit_borrowed_bytes(self.take(n)?)
            }
            SEQ => {
                let n = self.len()?;
                self.seq(Some(n), visitor)
            }
            MAP => {
                let n = self.len()?;
                self.map(Some(n), visitor)
            }
            SEQ_SMALL..=0x5F => self.seq(Some(usize::from(tag & 0x0F)), visitor),
            MAP_SMALL..=0x6F => self.map(Some(usize::from(tag & 0x0F)), visitor),
            SEQ_OPEN => self.seq(None, visitor),
            MAP_OPEN => self.map(None, visitor),
            UINT_SMALL..=0xFF => visitor.visit_u64(u64::from(tag & 0x7F)),
            _ => Err(unknown_tag(tag)),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.peek()? {
            UNIT => {
                self.pos += 1;
                visitor.visit_none()
            }
            SOME => {
                self.pos += 1;
                self.nested(|r| visitor.visit_some(r))
            }
            // A bare value where an `Option` is expected, as when a field
            // was changed from `T` to `Option<T>`.
            _ => self.nested(|r| visitor.visit_some(r)),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let tag = self.byte()?;
        if let Some(variant) = self.string(tag)? {
            return visitor.visit_enum(de::value::BorrowedStrDeserializer::new(variant));
        }
        let entries = match tag {
            MAP_SMALL..=0x6F => usize::from(tag & 0x0F),
            MAP => self.len()?,
            _ => return Err(BinaryError::new("expected an enum: a name, or a map with one entry")),
        };
        if entries != 1 {
            return Err(BinaryError::new(format!("expected an enum: a map with one entry, not {entries}")));
        }
        self.nested(|r| visitor.visit_enum(Variant { r }))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.skip()?;
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::fmt::Debug;

    fn encode<T: Serialize + ?Sized>(names: &mut Names, value: &T) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        value.serialize(&mut Writer { out: &mut out, names, key: false })?;
        Ok(out)
    }

    fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T> {
        let mut r = Reader::new(bytes, &[]);
        let value = T::deserialize(&mut r)?;
        assert_eq!(r.pos, bytes.len(), "all of the input is read");
        Ok(value)
    }

    #[track_caller]
    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + Debug>(value: T) -> Vec<u8> {
        let bytes = encode(&mut Names::default(), &value).unwrap();
        assert_eq!(decode::<T>(&bytes).unwrap(), value, "{bytes:02X?}");
        // Skipping reads exactly the same bytes.
        decode::<de::IgnoredAny>(&bytes).unwrap();
        bytes
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Unit;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Newtype(u8);

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Tuple(i8, String);

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Shape {
        Unit,
        Newtype(i32),
        Tuple(u8, Option<String>),
        Struct { name: String, sides: Vec<u16> },
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Everything {
        unit: (),
        unit_struct: Unit,
        newtype: Newtype,
        tuple: Tuple,
        shapes: Vec<Shape>,
        maybe: Option<Option<bool>>,
        chars: (char, char),
        floats: (f32, f64),
        by_number: BTreeMap<u32, String>,
        by_pair: BTreeMap<(u8, u8), ()>,
        by_name: BTreeMap<String, i64>,
        #[serde(flatten)]
        rest: BTreeMap<String, u8>,
    }

    fn everything() -> Everything {
        Everything {
            unit: (),
            unit_struct: Unit,
            newtype: Newtype(200),
            tuple: Tuple(-128, "a string that is longer than fifteen bytes".into()),
            shapes: vec![
                Shape::Unit,
                Shape::Newtype(-1),
                Shape::Tuple(7, None),
                Shape::Tuple(8, Some("x".into())),
                Shape::Struct { name: "sq".into(), sides: vec![1, 2, 3, 4] },
                Shape::Unit,
            ],
            maybe: Some(None),
            chars: ('a', 'ß'),
            floats: (1.5, -0.25),
            by_number: (0..20).map(|i| (i * 1000, i.to_string())).collect(),
            by_pair: [((1, 2), ()), ((3, 4), ())].into_iter().collect(),
            by_name: [("x".to_string(), i64::MIN), ("y".to_string(), i64::MAX)].into_iter().collect(),
            rest: [("extra".to_string(), 1)].into_iter().collect(),
        }
    }

    #[test]
    fn integers() {
        for v in [0, 1, 127, 128, 255, 16_383, 16_384, u64::MAX] {
            round_trip(v);
        }
        for v in [0, -1, -128, 127, i64::MIN, i64::MAX] {
            round_trip(v);
        }
        for v in [0, u128::from(u64::MAX) + 1, u128::MAX] {
            round_trip(v);
        }
        for v in [0, i128::from(i64::MIN) - 1, i128::MIN, i128::MAX] {
            round_trip(v);
        }
        assert_eq!(round_trip(127u64), [0xFF]);
        assert_eq!(round_trip(128u64), [UINT, 0x80, 0x01]);
        assert_eq!(round_trip(-1i64), [NEG, 0x00]);
        // Widening keeps old values readable.
        assert_eq!(decode::<u64>(&encode(&mut Names::default(), &200u8).unwrap()).unwrap(), 200);
        assert!(decode::<u8>(&encode(&mut Names::default(), &300u16).unwrap()).is_err());
    }

    #[test]
    fn every_shape_round_trips() {
        round_trip(everything());
        round_trip(Some(Some(Some(3u8))));
        round_trip(Option::<u8>::None);
        round_trip(String::new());
        round_trip("é".repeat(100));
    }

    #[test]
    fn names_are_written_once() {
        let mut names = Names::default();
        let first = encode(&mut names, &Shape::Struct { name: "a".into(), sides: vec![] }).unwrap();
        let again = encode(&mut names, &Shape::Struct { name: "a".into(), sides: vec![] }).unwrap();
        // Map, then variant, name and sides, each defined the first time...
        assert_eq!(first[..3], [MAP_SMALL | 1, NAME_DEF, 6]);
        // ...and one byte after that.
        assert_eq!(
            again,
            [MAP_SMALL | 1, NAME_SMALL, MAP_SMALL | 2, NAME_SMALL + 1, STR_SMALL | 1, b'a', NAME_SMALL + 2, SEQ_SMALL]
        );
    }

    #[test]
    fn many_names() {
        // Past the 48 that fit in a tag byte, and keys too long to be names.
        let map: BTreeMap<String, u8> =
            (0..100).map(|i| (format!("key {i}"), i)).chain([("k".repeat(100), 0)]).collect();
        let mut names = Names::default();
        let bytes = encode(&mut names, &(&map, &map)).unwrap();
        assert_eq!(names.len(), 100);
        assert_eq!(decode::<(BTreeMap<String, u8>, BTreeMap<String, u8>)>(&bytes).unwrap(), (map.clone(), map));
    }

    #[test]
    fn a_wrong_length_is_an_error() {
        struct Liar;
        impl Serialize for Liar {
            fn serialize<S: ser::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                use ser::SerializeSeq;
                let mut seq = s.serialize_seq(Some(3))?;
                seq.serialize_element(&1)?;
                seq.end()
            }
        }
        let err = encode(&mut Names::default(), &Liar).unwrap_err();
        assert!(err.0.contains("said it had 3 items, but had 1"), "{err}");
    }

    #[test]
    fn bad_input_is_an_error() {
        // Cut off anywhere: an error, never a panic.
        let bytes = encode(&mut Names::default(), &everything()).unwrap();
        for end in 0..bytes.len() {
            assert!(decode::<Everything>(&bytes[..end]).is_err(), "cut off at {end}");
            let mut r = Reader::new(&bytes[..end], &[]);
            assert!(r.skip().is_err(), "skipping, cut off at {end}");
        }
        assert!(decode::<String>(&[NAME_SMALL + 3]).unwrap_err().0.contains("name #3 is used before"));
        assert!(decode::<String>(&[STR_SMALL | 2, 0xFF, 0xFE]).unwrap_err().0.contains("UTF-8"));
        assert!(decode::<u8>(&[0x11]).unwrap_err().0.contains("unknown tag 0x11"));
        assert!(decode::<u64>(&[UINT, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]).is_err());
        let deep = [vec![SEQ_SMALL | 1; 200], vec![UINT_SMALL]].concat();
        assert!(decode::<de::IgnoredAny>(&deep).unwrap_err().0.contains("nested"));
        // Items left over that the visitor didn't ask for.
        assert!(decode::<(u8,)>(&[SEQ_SMALL | 2, UINT_SMALL, UINT_SMALL]).unwrap_err().0.contains("more items"));
    }

    #[test]
    fn skipped_values_still_define_names() {
        let mut names = Names::default();
        let mut bytes = encode(&mut names, &Shape::Struct { name: "a".into(), sides: vec![] }).unwrap();
        bytes.extend(encode(&mut names, &Shape::Unit).unwrap());
        bytes.extend(encode(&mut names, &Shape::Struct { name: "b".into(), sides: vec![5] }).unwrap());
        let mut r = Reader::new(&bytes, &[]);
        r.skip().unwrap();
        assert_eq!(Shape::deserialize(&mut r).unwrap(), Shape::Unit);
        assert_eq!(Shape::deserialize(&mut r).unwrap(), Shape::Struct { name: "b".into(), sides: vec![5] });
    }

    crate::model! {
        pub World {
            Thing { name: String, tags: crate::Map<String, u8> }
            Other { n: u8 }
        }
    }

    #[test]
    fn small_chunks_read_the_same_events() {
        let mut store = crate::Store::<World>::new();
        for i in 0..50 {
            let t = store.create(Thing { name: "x".repeat(i), ..Default::default() });
            // A new name in every event, so that an event read again after
            // more input defines names again.
            store.edit(t).tags.insert(format!("tag {i}"), i as u8);
        }
        let bytes = store.to_binary().unwrap();
        fn read(input: Input<'_>, chunk: usize) -> Vec<(Timestamp, WorldEvent)> {
            let mut events = Vec::new();
            read_chunked::<World>(input, chunk, false, |at, parsed| {
                if let Parsed::Event(e) = parsed {
                    events.push((at, e));
                }
                Ok(())
            })
            .unwrap();
            events
        }
        let whole = read(Input::Bytes(&bytes), CHUNK);
        assert_eq!(whole.len(), store.events().len());
        for chunk in [1, 2, 3, 7, 64] {
            let mut stream = &bytes[..];
            assert_eq!(read(Input::Stream(&mut stream), chunk), whole, "chunks of {chunk}");
        }
    }

    #[test]
    fn a_bare_value_is_read_as_some() {
        let bytes = encode(&mut Names::default(), &5u8).unwrap();
        assert_eq!(decode::<Option<u8>>(&bytes).unwrap(), Some(5));
    }
}
