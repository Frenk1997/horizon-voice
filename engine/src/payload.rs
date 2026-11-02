//! The plugin-channel messages carried by the Minecraft connection.

use crate::Version;
use crate::buf::{self, Reader, Uuid, Writer};
use crate::crypto::SECRET_LEN;

pub const REQUEST_SECRET: &str = "voicechat:request_secret";
pub const SECRET: &str = "voicechat:secret";
/// The client's own state: `update_state` in 20, `client_state` in 21.
pub const UPDATE_STATE_V20: &str = "voicechat:update_state";
pub const CLIENT_STATE_V21: &str = "voicechat:client_state";
pub const PLAYER_STATE: &str = "voicechat:state";
pub const PLAYER_STATES: &str = "voicechat:states";
pub const REMOVE_PLAYER_STATE: &str = "voicechat:remove_state";
pub const REGISTER: &str = "minecraft:register";

/// The channels this client announces with `minecraft:register`, so a
/// Bukkit/Paper server delivers them.
pub const CHANNELS: &[&str] = &[
    REQUEST_SECRET,
    SECRET,
    UPDATE_STATE_V20,
    CLIENT_STATE_V21,
    PLAYER_STATE,
    PLAYER_STATES,
    REMOVE_PLAYER_STATE,
    crate::groups::ADD_GROUP,
    crate::groups::REMOVE_GROUP,
    crate::groups::JOINED_GROUP,
    crate::groups::CREATE_GROUP,
    crate::groups::JOIN_GROUP,
    crate::groups::LEAVE_GROUP,
];

/// `minecraft:register`: the names, NUL-separated.
pub fn register() -> Vec<u8> {
    CHANNELS.join("\0").into_bytes()
}

pub fn request_secret(version: Version) -> Vec<u8> {
    let mut w = Writer::new();
    w.i32(version.number());
    w.into_inner()
}

/// The channel and body announcing whether this client is muted or has voice
/// chat off. Version 20 knows only `disabled`.
pub fn client_state(version: Version, muted: bool, disabled: bool) -> (&'static str, Vec<u8>) {
    let mut w = Writer::new();
    match version {
        Version::V20 => {
            w.bool(disabled);
            (UPDATE_STATE_V20, w.into_inner())
        }
        Version::V21 => {
            w.bool(muted).bool(disabled);
            (CLIENT_STATE_V21, w.into_inner())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Voip,
    Audio,
    RestrictedLowDelay,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SecretInfo {
    pub version: Version,
    pub secret: [u8; SECRET_LEN],
    /// Version 21's id for the UDP header; version 20 uses the player's UUID.
    pub connection_id: Option<Uuid>,
    pub server_port: i32,
    pub player: Uuid,
    pub codec: Codec,
    pub mtu: i32,
    pub distance: f64,
    /// Milliseconds between keep-alives.
    pub keep_alive: i32,
    pub groups: bool,
    pub voice_host: String,
    pub allow_recording: bool,
}

impl SecretInfo {
    pub fn decode(data: &[u8], version: Version) -> buf::Result<SecretInfo> {
        let mut r = Reader::new(data);
        let secret = r.bytes(SECRET_LEN)?.try_into().unwrap();
        let connection_id = match version {
            Version::V20 => None,
            Version::V21 => Some(r.uuid()?),
        };
        Ok(SecretInfo {
            version,
            secret,
            connection_id,
            server_port: r.i32()?,
            player: r.uuid()?,
            codec: match r.u8()? {
                1 => Codec::Audio,
                2 => Codec::RestrictedLowDelay,
                _ => Codec::Voip,
            },
            mtu: r.i32()?,
            distance: r.f64()?,
            keep_alive: r.i32()?,
            groups: r.bool()?,
            voice_host: r.string(32767)?,
            allow_recording: r.bool()?,
        })
    }

    /// The id in front of every datagram to the server.
    pub fn header_id(&self) -> Uuid {
        self.connection_id.unwrap_or(self.player)
    }

    /// Where to send voice: `voice_host` (`host` or `host:port`) when set,
    /// else the Minecraft server's host with `server_port`.
    pub fn voice_address(&self, minecraft_host: &str) -> (String, u16) {
        let port = self.server_port as u16;
        let host = self.voice_host.trim();
        if host.is_empty() {
            return (minecraft_host.to_owned(), port);
        }
        // `[v6]:port`, `host:port` (one colon), or a bare host / bare IPv6.
        if let Some(rest) = host.strip_prefix('[') {
            if let Some((addr, tail)) = rest.split_once(']') {
                let p = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(port);
                return (addr.to_owned(), p);
            }
        }
        match host.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_owned(), p.parse().unwrap_or(port)),
            _ => (host.to_owned(), port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(version: Version, voice_host: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.bytes(&[0x11; 16]);
        if version == Version::V21 {
            w.uuid(7);
        }
        w.i32(24454).uuid(9).u8(0).i32(1024).f64(48.0).i32(1000).bool(true);
        w.string(voice_host).bool(false);
        w.into_inner()
    }

    #[test]
    fn decodes_secret_packet_v21() {
        let s = SecretInfo::decode(&sample(Version::V21, ""), Version::V21).unwrap();
        assert_eq!(s.secret, [0x11; 16]);
        assert_eq!(s.connection_id, Some(7));
        assert_eq!(s.header_id(), 7);
        assert_eq!(s.server_port, 24454);
        assert_eq!(s.player, 9);
        assert_eq!(s.codec, Codec::Voip);
        assert_eq!(s.mtu, 1024);
        assert_eq!(s.distance, 48.0);
        assert_eq!(s.keep_alive, 1000);
        assert!(s.groups);
        assert!(!s.allow_recording);
    }

    #[test]
    fn decodes_secret_packet_v20() {
        let s = SecretInfo::decode(&sample(Version::V20, "v:1"), Version::V20).unwrap();
        assert_eq!(s.connection_id, None);
        assert_eq!(s.header_id(), 9, "the player's UUID");
        assert_eq!(s.server_port, 24454);
        assert_eq!(s.voice_host, "v:1");
    }

    #[test]
    fn voice_address_rules() {
        let addr = |h: &str| SecretInfo::decode(&sample(Version::V20, h), Version::V20).unwrap().voice_address("mc.example");
        assert_eq!(addr(""), ("mc.example".into(), 24454));
        assert_eq!(addr("voice.example"), ("voice.example".into(), 24454));
        assert_eq!(addr("voice.example:5000"), ("voice.example".into(), 5000));
        assert_eq!(addr("[::1]:6000"), ("::1".into(), 6000));
        assert_eq!(addr("::1"), ("::1".into(), 24454));
    }

    #[test]
    fn register_lists_channels_nul_separated() {
        let r = String::from_utf8(register()).unwrap();
        assert!(r.split('\0').any(|c| c == SECRET));
        assert!(!r.ends_with('\0'));
    }

    #[test]
    fn request_secret_is_a_big_endian_int() {
        assert_eq!(request_secret(Version::V20), [0, 0, 0, 20]);
        assert_eq!(request_secret(Version::V21), [0, 0, 0, 21]);
    }

    #[test]
    fn client_state_per_version() {
        assert_eq!(client_state(Version::V20, true, false), (UPDATE_STATE_V20, vec![0]));
        assert_eq!(client_state(Version::V21, true, false), (CLIENT_STATE_V21, vec![1, 0]));
    }
}
