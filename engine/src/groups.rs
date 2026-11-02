//! Simple Voice Chat's groups: the plugin messages that list them, join and
//! leave them, and the player states that say who is in which.
//!
//! Inside a group the server sends the members' voices as group sound (heard
//! at full volume wherever they are); the microphone packets don't change.

use crate::Version;
use crate::buf::{self, Reader, Uuid, Writer};

pub const ADD_GROUP: &str = "voicechat:add_group";
pub const REMOVE_GROUP: &str = "voicechat:remove_group";
pub const JOINED_GROUP: &str = "voicechat:joined_group";
pub const CREATE_GROUP: &str = "voicechat:create_group";
pub const JOIN_GROUP: &str = "voicechat:set_group";
pub const LEAVE_GROUP: &str = "voicechat:leave_group";

pub const CHANNELS: &[&str] = &[ADD_GROUP, REMOVE_GROUP, JOINED_GROUP, CREATE_GROUP, JOIN_GROUP, LEAVE_GROUP];

/// A group name or password: at most 24 characters.
pub const MAX_NAME: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupType {
    /// Members hear each other, and the players near them.
    Normal,
    /// Players near the members hear them too.
    Open,
    /// Members hear only each other.
    Isolated,
}

impl GroupType {
    fn from_wire(n: i16) -> GroupType {
        match n {
            1 => GroupType::Open,
            2 => GroupType::Isolated,
            _ => GroupType::Normal,
        }
    }

    fn to_wire(self) -> i16 {
        match self {
            GroupType::Normal => 0,
            GroupType::Open => 1,
            GroupType::Isolated => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub has_password: bool,
    pub persistent: bool,
    pub hidden: bool,
    pub kind: GroupType,
}

impl Group {
    pub fn decode(data: &[u8]) -> buf::Result<Group> {
        let mut r = Reader::new(data);
        Ok(Group {
            id: r.uuid()?,
            name: r.string(512)?,
            has_password: r.bool()?,
            persistent: r.bool()?,
            hidden: r.bool()?,
            kind: GroupType::from_wire(r.i16()?),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub uuid: Uuid,
    pub name: String,
    pub disabled: bool,
    pub disconnected: bool,
    pub group: Option<Uuid>,
}

impl PlayerState {
    /// Version 20's layout. Version 21 added fields (muted, permissions), so
    /// its states are not read.
    fn read(r: &mut Reader<'_>) -> buf::Result<PlayerState> {
        let disabled = r.bool()?;
        let disconnected = r.bool()?;
        let uuid = r.uuid()?;
        let name = r.string(32767)?;
        let group = if r.bool()? { Some(r.uuid()?) } else { None };
        Ok(PlayerState { uuid, name, disabled, disconnected, group })
    }

    pub fn decode_one(data: &[u8], version: Version) -> Option<PlayerState> {
        (version == Version::V20).then(|| PlayerState::read(&mut Reader::new(data)).ok()).flatten()
    }

    pub fn decode_all(data: &[u8], version: Version) -> Option<Vec<PlayerState>> {
        if version != Version::V20 {
            return None;
        }
        let mut r = Reader::new(data);
        let count = r.i32().ok()?;
        (0..count.max(0)).map(|_| PlayerState::read(&mut r).ok()).collect()
    }
}

/// `voicechat:joined_group`: the group the player is in now (none: left),
/// and whether a join failed on its password.
pub fn decode_joined(data: &[u8]) -> buf::Result<(Option<Uuid>, bool)> {
    let mut r = Reader::new(data);
    let group = if r.bool()? { Some(r.uuid()?) } else { None };
    Ok((group, r.bool()?))
}

pub fn decode_removed(data: &[u8]) -> buf::Result<Uuid> {
    Reader::new(data).uuid()
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_NAME).collect()
}

pub fn create(name: &str, password: Option<&str>, kind: GroupType) -> Vec<u8> {
    let mut w = Writer::new();
    w.string(&clip(name));
    match password.filter(|p| !p.is_empty()) {
        Some(p) => {
            w.bool(true).string(&clip(p));
        }
        None => {
            w.bool(false);
        }
    }
    w.i16(kind.to_wire());
    w.into_inner()
}

pub fn join(group: Uuid, password: Option<&str>) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(group);
    match password.filter(|p| !p.is_empty()) {
        Some(p) => {
            w.bool(true).string(p);
        }
        None => {
            w.bool(false);
        }
    }
    w.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_wire_format() {
        let mut w = Writer::new();
        w.uuid(5).string("Base").bool(true).bool(false).bool(false).i16(2);
        let g = Group::decode(&w.data).unwrap();
        assert_eq!(
            g,
            Group { id: 5, name: "Base".into(), has_password: true, persistent: false, hidden: false, kind: GroupType::Isolated }
        );
    }

    #[test]
    fn states_and_joined() {
        let mut w = Writer::new();
        w.i32(2);
        w.bool(false).bool(false).uuid(1).string("Steve").bool(true).uuid(9);
        w.bool(true).bool(false).uuid(2).string("Alex").bool(false);
        let all = PlayerState::decode_all(&w.data, Version::V20).unwrap();
        assert_eq!(all[0].group, Some(9));
        assert_eq!(all[1].name, "Alex");
        assert!(all[1].disabled);
        assert!(PlayerState::decode_all(&w.data, Version::V21).is_none());

        let mut j = Writer::new();
        j.bool(true).uuid(9).bool(false);
        assert_eq!(decode_joined(&j.data).unwrap(), (Some(9), false));
        assert_eq!(decode_joined(&[0, 1]).unwrap(), (None, true));
    }

    #[test]
    fn create_and_join_encode() {
        let c = create("A very long group name that goes on", Some("pw"), GroupType::Open);
        let mut r = Reader::new(&c);
        assert_eq!(r.string(24).unwrap().chars().count(), 24);
        assert!(r.bool().unwrap());
        assert_eq!(r.string(24).unwrap(), "pw");
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(create("x", Some(""), GroupType::Normal), [1, b'x', 0, 0, 0]);

        let j = join(7, None);
        assert_eq!(j.len(), 17);
        assert_eq!(j[16], 0);
    }
}
