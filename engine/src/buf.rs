//! Minecraft's `FriendlyByteBuf` encoding: big-endian numbers, VarInt
//! lengths, UUIDs as two longs.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Eof,
    VarIntTooBig,
    TooLong { len: usize, max: usize },
    Utf8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Eof => write!(f, "unexpected end of data"),
            Error::VarIntTooBig => write!(f, "VarInt too big"),
            Error::TooLong { len, max } => write!(f, "length {len} over the limit {max}"),
            Error::Utf8 => write!(f, "invalid UTF-8"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub type Uuid = u128;

pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    pub fn remaining(&self) -> usize {
        self.data.len()
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.data.len() < n {
            return Err(Error::Eof);
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().unwrap())
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }

    pub fn uuid(&mut self) -> Result<Uuid> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    pub fn varint(&mut self) -> Result<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            value |= ((b & 0x7f) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(Error::VarIntTooBig)
    }

    fn length(&mut self, max: usize) -> Result<usize> {
        let len = self.varint()?;
        let len = usize::try_from(len).map_err(|_| Error::TooLong { len: usize::MAX, max })?;
        if len > max {
            return Err(Error::TooLong { len, max });
        }
        Ok(len)
    }

    pub fn byte_array(&mut self, max: usize) -> Result<Vec<u8>> {
        let len = self.length(max)?;
        Ok(self.bytes(len)?.to_vec())
    }

    /// `readUtf(max)`: at most `max` characters, at most `max * 3` bytes.
    pub fn string(&mut self, max: usize) -> Result<String> {
        let len = self.length(max * 3)?;
        let s = std::str::from_utf8(self.bytes(len)?).map_err(|_| Error::Utf8)?;
        if s.encode_utf16().count() > max {
            return Err(Error::TooLong { len: s.len(), max });
        }
        Ok(s.to_owned())
    }
}

#[derive(Default)]
pub struct Writer {
    pub data: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.data.extend_from_slice(b);
        self
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.bytes(&[v])
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub fn uuid(&mut self, v: Uuid) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub fn varint(&mut self, v: i32) -> &mut Self {
        let mut v = v as u32;
        loop {
            if v & !0x7f == 0 {
                return self.u8(v as u8);
            }
            self.u8((v & 0x7f) as u8 | 0x80);
            v >>= 7;
        }
    }

    pub fn byte_array(&mut self, b: &[u8]) -> &mut Self {
        self.varint(b.len() as i32).bytes(b)
    }

    pub fn string(&mut self, s: &str) -> &mut Self {
        self.byte_array(s.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips() {
        for v in [0, 1, 127, 128, 255, 25565, 2097151, i32::MAX, -1, i32::MIN] {
            let mut w = Writer::new();
            w.varint(v);
            assert_eq!(Reader::new(&w.data).varint().unwrap(), v, "{v}");
        }
    }

    #[test]
    fn varint_matches_minecraft_bytes() {
        let mut w = Writer::new();
        w.varint(300).varint(-1);
        assert_eq!(w.data, [0xac, 0x02, 0xff, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn uuid_is_two_big_endian_longs() {
        let id: Uuid = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;
        let mut w = Writer::new();
        w.uuid(id);
        assert_eq!(w.data[0], 0x00);
        assert_eq!(w.data[15], 0xff);
        assert_eq!(Reader::new(&w.data).uuid().unwrap(), id);
    }

    #[test]
    fn byte_array_limit_is_enforced() {
        let mut w = Writer::new();
        w.byte_array(&[1, 2, 3]);
        assert!(matches!(
            Reader::new(&w.data).byte_array(2),
            Err(Error::TooLong { len: 3, max: 2 })
        ));
    }
}
