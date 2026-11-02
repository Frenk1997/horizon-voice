//! The UDP voice packets and their framing.
//!
//! Client → server: `0xFF`, connection id (UUID), byte array of the encrypted
//! payload. Server → client: `0xFF`, byte array of the encrypted payload. The
//! decrypted payload is a type byte and the packet's body.

use crate::{MAX_OPUS_PAYLOAD, Version};
use crate::buf::{self, Reader, Uuid, Writer};
use crate::crypto::{CryptoError, SECRET_LEN, Secret};

pub const MAGIC: u8 = 0xFF;
pub const NONCE_LEN: usize = 16;
/// Upper bound for an encrypted payload on the wire.
pub const MAX_PACKET: usize = 4096;

const WHISPER: u8 = 0b01;
const HAS_CATEGORY: u8 = 0b10;

#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub channel: Uuid,
    pub sender: Uuid,
    /// Opus, or raw s16le PCM when `sequence < 0`.
    pub data: Vec<u8>,
    pub sequence: i64,
    pub category: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Mic { data: Vec<u8>, sequence: i64, whispering: bool },
    PlayerSound { sound: Sound, distance: f32, whispering: bool },
    GroupSound { sound: Sound },
    LocationSound { sound: Sound, location: [f64; 3], distance: f32 },
    /// Version 20 sends the player's UUID and the secret; 21 sends nothing.
    Authenticate { credentials: Option<(Uuid, [u8; SECRET_LEN])> },
    /// The nonces are version 21's; version 20's packets are empty.
    AuthenticateAck { nonce: Option<[u8; NONCE_LEN]> },
    Ping { id: Uuid, timestamp: i64 },
    KeepAlive,
    ConnectionCheck { nonce: Option<[u8; NONCE_LEN]> },
    ConnectionCheckAck,
}

#[derive(Debug)]
pub enum Error {
    Buf(buf::Error),
    Crypto,
    BadMagic,
    UnknownType(u8),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Buf(e) => write!(f, "malformed packet: {e}"),
            Error::Crypto => write!(f, "could not decrypt"),
            Error::BadMagic => write!(f, "not a voice chat packet"),
            Error::UnknownType(t) => write!(f, "unknown packet type {t:#x}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<buf::Error> for Error {
    fn from(e: buf::Error) -> Self {
        Error::Buf(e)
    }
}

impl From<CryptoError> for Error {
    fn from(_: CryptoError) -> Self {
        Error::Crypto
    }
}

fn read_flags_category(r: &mut Reader<'_>) -> buf::Result<(u8, Option<String>)> {
    let flags = r.u8()?;
    let category = if flags & HAS_CATEGORY != 0 { Some(r.string(16)?) } else { None };
    Ok((flags, category))
}

fn write_flags_category(w: &mut Writer, mut flags: u8, category: &Option<String>) {
    if category.is_some() {
        flags |= HAS_CATEGORY;
    }
    w.u8(flags);
    if let Some(c) = category {
        w.string(c);
    }
}

fn nonce(r: &mut Reader<'_>, version: Version) -> buf::Result<Option<[u8; NONCE_LEN]>> {
    Ok(match version {
        Version::V20 => None,
        Version::V21 => Some(r.bytes(NONCE_LEN)?.try_into().unwrap()),
    })
}

impl Packet {
    pub fn type_id(&self) -> u8 {
        match self {
            Packet::Mic { .. } => 0x1,
            Packet::PlayerSound { .. } => 0x2,
            Packet::GroupSound { .. } => 0x3,
            Packet::LocationSound { .. } => 0x4,
            Packet::Authenticate { .. } => 0x5,
            Packet::AuthenticateAck { .. } => 0x6,
            Packet::Ping { .. } => 0x7,
            Packet::KeepAlive => 0x8,
            Packet::ConnectionCheck { .. } => 0x9,
            Packet::ConnectionCheckAck => 0xA,
        }
    }

    /// The decrypted payload: type byte and body.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(self.type_id());
        match self {
            Packet::Mic { data, sequence, whispering } => {
                w.byte_array(data).i64(*sequence).bool(*whispering);
            }
            Packet::PlayerSound { sound, distance, whispering } => {
                w.uuid(sound.channel).uuid(sound.sender).byte_array(&sound.data).i64(sound.sequence).f32(*distance);
                write_flags_category(&mut w, if *whispering { WHISPER } else { 0 }, &sound.category);
            }
            Packet::GroupSound { sound } => {
                w.uuid(sound.channel).uuid(sound.sender).byte_array(&sound.data).i64(sound.sequence);
                write_flags_category(&mut w, 0, &sound.category);
            }
            Packet::LocationSound { sound, location, distance } => {
                w.uuid(sound.channel).uuid(sound.sender);
                w.f64(location[0]).f64(location[1]).f64(location[2]);
                w.byte_array(&sound.data).i64(sound.sequence).f32(*distance);
                write_flags_category(&mut w, 0, &sound.category);
            }
            Packet::Authenticate { credentials } => {
                if let Some((player, secret)) = credentials {
                    w.uuid(*player).bytes(secret);
                }
            }
            Packet::AuthenticateAck { nonce } | Packet::ConnectionCheck { nonce } => {
                if let Some(nonce) = nonce {
                    w.bytes(nonce);
                }
            }
            Packet::Ping { id, timestamp } => {
                w.uuid(*id).i64(*timestamp);
            }
            Packet::KeepAlive | Packet::ConnectionCheckAck => {}
        }
        w.into_inner()
    }

    pub fn decode(payload: &[u8], version: Version) -> Result<Packet, Error> {
        let mut r = Reader::new(payload);
        let kind = r.u8()?;
        let sound_head = |r: &mut Reader<'_>| -> buf::Result<(Uuid, Uuid)> { Ok((r.uuid()?, r.uuid()?)) };
        Ok(match kind {
            0x1 => {
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                Packet::Mic { data, sequence: r.i64()?, whispering: r.bool()? }
            }
            0x2 => {
                let (channel, sender) = sound_head(&mut r)?;
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                let sequence = r.i64()?;
                let distance = r.f32()?;
                let (flags, category) = read_flags_category(&mut r)?;
                Packet::PlayerSound {
                    sound: Sound { channel, sender, data, sequence, category },
                    distance,
                    whispering: flags & WHISPER != 0,
                }
            }
            0x3 => {
                let (channel, sender) = sound_head(&mut r)?;
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                let sequence = r.i64()?;
                let (_, category) = read_flags_category(&mut r)?;
                Packet::GroupSound { sound: Sound { channel, sender, data, sequence, category } }
            }
            0x4 => {
                let (channel, sender) = sound_head(&mut r)?;
                let location = [r.f64()?, r.f64()?, r.f64()?];
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                let sequence = r.i64()?;
                let distance = r.f32()?;
                let (_, category) = read_flags_category(&mut r)?;
                Packet::LocationSound { sound: Sound { channel, sender, data, sequence, category }, location, distance }
            }
            0x5 => Packet::Authenticate {
                credentials: match version {
                    Version::V20 => Some((r.uuid()?, r.bytes(SECRET_LEN)?.try_into().unwrap())),
                    Version::V21 => None,
                },
            },
            0x6 => Packet::AuthenticateAck { nonce: nonce(&mut r, version)? },
            0x7 => Packet::Ping { id: r.uuid()?, timestamp: r.i64()? },
            0x8 => Packet::KeepAlive,
            0x9 => Packet::ConnectionCheck { nonce: nonce(&mut r, version)? },
            0xA => Packet::ConnectionCheckAck,
            other => return Err(Error::UnknownType(other)),
        })
    }
}

/// A datagram from this client to the server.
pub fn write_client(secret: &Secret, connection: Uuid, packet: &Packet) -> Result<Vec<u8>, Error> {
    let enc = secret.encrypt(&packet.encode())?;
    let mut w = Writer::new();
    w.u8(MAGIC).uuid(connection).byte_array(&enc);
    Ok(w.into_inner())
}

/// A datagram from the server to this client.
pub fn read_client(secret: &Secret, datagram: &[u8], version: Version) -> Result<Packet, Error> {
    let mut r = Reader::new(datagram);
    if r.u8()? != MAGIC {
        return Err(Error::BadMagic);
    }
    let enc = r.byte_array(MAX_PACKET)?;
    Packet::decode(&secret.decrypt(&enc)?, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(category: Option<&str>) -> Sound {
        Sound { channel: 1, sender: 2, data: vec![9, 8, 7], sequence: 42, category: category.map(Into::into) }
    }

    fn all_v21() -> Vec<Packet> {
        vec![
            Packet::Mic { data: vec![1, 2, 3], sequence: 5, whispering: true },
            Packet::PlayerSound { sound: sound(None), distance: 48.0, whispering: true },
            Packet::PlayerSound { sound: sound(Some("music")), distance: 8.0, whispering: false },
            Packet::GroupSound { sound: sound(Some("g")) },
            Packet::LocationSound { sound: sound(None), location: [1.5, -64.0, 1e6], distance: 16.0 },
            Packet::Authenticate { credentials: None },
            Packet::AuthenticateAck { nonce: Some([3; 16]) },
            Packet::Ping { id: 77, timestamp: -9 },
            Packet::KeepAlive,
            Packet::ConnectionCheck { nonce: Some([4; 16]) },
            Packet::ConnectionCheckAck,
        ]
    }

    #[test]
    fn every_packet_round_trips() {
        for p in all_v21() {
            assert_eq!(Packet::decode(&p.encode(), Version::V21).unwrap(), p);
        }
        let v20 = [
            Packet::Authenticate { credentials: Some((0xAB, [5; 16])) },
            Packet::AuthenticateAck { nonce: None },
            Packet::ConnectionCheck { nonce: None },
            Packet::Mic { data: vec![1], sequence: 0, whispering: false },
        ];
        for p in v20 {
            assert_eq!(Packet::decode(&p.encode(), Version::V20).unwrap(), p);
        }
    }

    #[test]
    fn v20_authenticate_layout() {
        let p = Packet::Authenticate { credentials: Some((1, [0xEE; 16])) };
        let b = p.encode();
        assert_eq!(b.len(), 1 + 16 + 16);
        assert_eq!(b[0], 0x5);
        assert_eq!(b[16], 1);
        assert_eq!(&b[17..], [0xEE; 16]);
        assert_eq!(Packet::AuthenticateAck { nonce: None }.encode(), [0x6]);
    }

    #[test]
    fn mic_packet_layout() {
        let p = Packet::Mic { data: vec![0xAA, 0xBB], sequence: 1, whispering: false };
        assert_eq!(p.encode(), [0x1, 2, 0xAA, 0xBB, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
    }

    #[test]
    fn player_sound_flags_byte() {
        let p = Packet::PlayerSound { sound: sound(Some("ab")), distance: 1.0, whispering: true };
        let bytes = p.encode();
        // type, 2 uuids, len+3 bytes, seq, f32 distance, then flags.
        let flags_at = 1 + 32 + 4 + 8 + 4;
        assert_eq!(bytes[flags_at], WHISPER | HAS_CATEGORY);
        assert_eq!(&bytes[flags_at + 1..], [2, b'a', b'b']);
    }

    #[test]
    fn client_framing_carries_connection_id() {
        let s = Secret::new([5; 16]);
        let d = write_client(&s, 0xABCD, &Packet::KeepAlive).unwrap();
        assert_eq!(d[0], MAGIC);
        assert_eq!(u128::from_be_bytes(d[1..17].try_into().unwrap()), 0xABCD);
    }

    #[test]
    fn server_framing_round_trips() {
        let s = Secret::new([6; 16]);
        let p = Packet::Ping { id: 1, timestamp: 2 };
        let mut w = Writer::new();
        w.u8(MAGIC).byte_array(&s.encrypt(&p.encode()).unwrap());
        assert_eq!(read_client(&s, &w.data, Version::V20).unwrap(), p);
        assert!(matches!(read_client(&s, &[0, 0], Version::V20), Err(Error::BadMagic)));
    }

    #[test]
    fn unknown_type_is_an_error() {
        assert!(matches!(Packet::decode(&[0x42], Version::V20), Err(Error::UnknownType(0x42))));
    }
}
