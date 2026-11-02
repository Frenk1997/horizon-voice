//! A voice chat client that speaks Simple Voice Chat's protocol, for Horizon.
//!
//! An independent implementation: no Simple Voice Chat code is used, only its
//! wire format (see PLAN.md). The game-facing glue lives in the mod's helpers;
//! everything here builds and tests without the game.

pub mod audio;
pub mod buf;
pub mod client;
pub mod codec;
pub mod crypto;
pub mod denoise;
pub mod groups;
pub mod i18n;
#[allow(dead_code, unused_imports, clippy::all)]
pub mod opus;
pub mod packets;
pub mod payload;
pub mod session;
pub mod settings;
pub mod voice;

/// Simple Voice Chat's compatibility version. The server refuses a client
/// whose version differs, and the handshake changed between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Version {
    /// The 2.6.x releases servers run.
    #[default]
    V20,
    /// The 2.7 development line: connection ids and handshake nonces.
    V21,
}

impl Version {
    pub fn number(self) -> i32 {
        match self {
            Version::V20 => 20,
            Version::V21 => 21,
        }
    }

    pub fn from_number(n: i32) -> Option<Version> {
        match n {
            20 => Some(Version::V20),
            21 => Some(Version::V21),
            _ => None,
        }
    }
}

/// Opus frame: 48 kHz mono, 20 ms.
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SIZE: usize = 960;
pub const MAX_OPUS_PAYLOAD: usize = 1275;
