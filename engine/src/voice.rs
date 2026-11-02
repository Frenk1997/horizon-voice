//! What the game drives: the voice chat for one server session.
//!
//! The game tells it when the player joins and leaves a server, hands it the
//! `voicechat:*` plugin messages, sends the ones it queues, and calls
//! [`Voice::frame`] every frame with where the player stands and a way to look
//! up other players' positions. Everything else (network, codec, sound card)
//! runs here.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::Version;
use crate::audio::{self, Capture, Mixer, Playback};
use crate::buf::Uuid;
use crate::client::VoiceClient;
use crate::codec::{Decoder, Encoder};
use crate::denoise::{Denoiser, Strength};
use crate::groups::{self, Group, GroupType, PlayerState};
use crate::i18n::Lang;
use crate::payload::{self, SecretInfo};
use crate::session::Event;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    PushToTalk,
    Voice,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub version: Version,
    pub activation: Activation,
    /// The push-to-talk key, as Horizon names keys (`key.keyboard.v`).
    pub ptt_key: String,
    /// The mute toggle key.
    pub mute_key: String,
    /// Voice activation starts above this level.
    pub threshold_db: f32,
    pub noise: Strength,
    pub mic_device: String,
    pub speaker_device: String,
    pub volume: f32,
    pub mic_gain: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: Version::V20,
            activation: Activation::PushToTalk,
            ptt_key: "key.keyboard.v".into(),
            mute_key: "key.keyboard.m".into(),
            threshold_db: -45.0,
            noise: Strength::Medium,
            mic_device: String::new(),
            speaker_device: String::new(),
            volume: 1.0,
            mic_gain: 1.0,
        }
    }
}

pub const CONFIG_FILE: &str = "horizon_voice.txt";

impl Config {
    /// `key=value` lines; `#` starts a comment; unknown keys and bad values
    /// keep the default.
    pub fn parse(text: &str) -> Config {
        let mut c = Config::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "compatibility_version" => {
                    if let Some(ver) = v.parse().ok().and_then(Version::from_number) {
                        c.version = ver;
                    }
                }
                "activation" => match v {
                    "ptt" | "push_to_talk" => c.activation = Activation::PushToTalk,
                    "voice" => c.activation = Activation::Voice,
                    _ => {}
                },
                "ptt_key" if !v.is_empty() => c.ptt_key = v.into(),
                "mute_key" if !v.is_empty() => c.mute_key = v.into(),
                "threshold_db" => c.threshold_db = v.parse::<f32>().map(|x| x.clamp(-90.0, 0.0)).unwrap_or(c.threshold_db),
                "noise_suppression" => c.noise = Strength::from_name(v).unwrap_or(c.noise),
                "mic_device" => c.mic_device = v.into(),
                "speaker_device" => c.speaker_device = v.into(),
                "volume" => c.volume = v.parse::<f32>().map(|x| x.clamp(0.0, 4.0)).unwrap_or(c.volume),
                "mic_gain" => c.mic_gain = v.parse::<f32>().map(|x| x.clamp(0.0, 8.0)).unwrap_or(c.mic_gain),
                _ => {}
            }
        }
        c
    }

    pub fn to_text(&self) -> String {
        format!(
            "# Horizon Voice (Simple Voice Chat compatible)\n\
             # 20 for Simple Voice Chat 2.6.x servers, 21 for 2.7\n\
             compatibility_version={}\n\
             # ptt (push to talk) or voice (voice activation)\n\
             activation={}\n\
             ptt_key={}\n\
             mute_key={}\n\
             threshold_db={}\n\
             # off, low, medium or high\n\
             noise_suppression={}\n\
             # empty: the system default\n\
             mic_device={}\n\
             speaker_device={}\n\
             volume={}\n\
             mic_gain={}\n",
            self.version.number(),
            match self.activation {
                Activation::PushToTalk => "ptt",
                Activation::Voice => "voice",
            },
            self.ptt_key,
            self.mute_key,
            self.threshold_db,
            self.noise.name(),
            self.mic_device,
            self.speaker_device,
            self.volume,
            self.mic_gain
        )
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(CONFIG_FILE), self.to_text())
    }

    /// Reads `dir/horizon_voice.txt`, writing the defaults there first if it
    /// doesn't exist yet.
    pub fn load(dir: &Path) -> Config {
        let path = dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text),
            Err(_) => {
                let c = Config::default();
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(&path, c.to_text());
                c
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Not on a server, or the server has no voice chat (yet).
    Idle,
    Connecting,
    Connected,
    Failed(String),
}

/// Where the player hears from.
#[derive(Debug, Clone, Copy)]
pub struct Listener {
    pub position: [f64; 3],
    pub yaw: f32,
}

/// How long voice activation keeps sending after the level drops: 500 ms.
const HANGOVER_FRAMES: u32 = 25;

pub struct Voice {
    pub config: Config,
    /// The language of the lines for the chat; the game sets it.
    pub lang: Lang,
    host: String,
    /// False while the game still looks the server's address up (its SRV
    /// redirect); a secret that arrives meanwhile waits in `pending`.
    host_ready: bool,
    pending: Option<SecretInfo>,
    info: Option<SecretInfo>,
    client: Option<VoiceClient>,
    connected: bool,
    status: Status,
    outgoing: Vec<(String, Vec<u8>)>,
    encoder: Option<Encoder>,
    decoders: HashMap<Uuid, Decoder>,
    mixer: Arc<Mutex<Mixer>>,
    capture: Option<Capture>,
    playback: Option<Playback>,
    audio_error: Option<String>,
    ptt: bool,
    muted: bool,
    hangover: u32,
    /// Speech is going out (until push-to-talk is let go or voice
    /// activation's hangover ends): what the HUD shows.
    sending: bool,
    /// Whether the last microphone frame was speech to send.
    wants: bool,
    denoiser: Denoiser,
    /// Hear yourself, processed as the others would, to tune the settings.
    mic_test: bool,
    /// The last microphone frame's level, after gain and noise suppression.
    level: f32,
    /// The server's groups, by id.
    groups: BTreeMap<Uuid, Group>,
    /// Voice chat's players and the group each is in.
    players: HashMap<Uuid, PlayerState>,
    my_group: Option<Uuid>,
    /// Lines for the chat (joined a group, a wrong password...).
    notices: Vec<String>,
}

/// The mixer's speaker for the microphone test.
const LOOPBACK: Uuid = 0;

impl Voice {
    pub fn new(config: Config) -> Voice {
        let mut mixer = Mixer::new();
        mixer.volume = config.volume;
        let denoiser = Denoiser::new(config.noise);
        Voice {
            config,
            lang: Lang::En,
            host: String::new(),
            host_ready: false,
            pending: None,
            info: None,
            client: None,
            connected: false,
            status: Status::Idle,
            outgoing: Vec::new(),
            encoder: None,
            decoders: HashMap::new(),
            mixer: Arc::new(Mutex::new(mixer)),
            capture: None,
            playback: None,
            audio_error: None,
            ptt: false,
            muted: false,
            hangover: 0,
            sending: false,
            wants: false,
            denoiser,
            mic_test: false,
            level: -127.0,
            groups: BTreeMap::new(),
            players: HashMap::new(),
            my_group: None,
            notices: Vec::new(),
        }
    }

    /// New settings, applied at once: volume, noise suppression, devices.
    /// The compatibility version counts from the next server joined.
    pub fn apply_config(&mut self, config: Config) {
        let devices = config.mic_device != self.config.mic_device || config.speaker_device != self.config.speaker_device;
        self.denoiser.strength = config.noise;
        if let Ok(mut m) = self.mixer.lock() {
            m.volume = config.volume;
        }
        self.config = config;
        if devices && (self.capture.is_some() || self.playback.is_some()) {
            self.close_audio();
            self.open_audio();
        }
    }

    pub fn set_mic_test(&mut self, on: bool) {
        self.mic_test = on;
        if on {
            if self.capture.is_none() && self.playback.is_none() {
                self.open_audio();
            }
        } else {
            if let Ok(mut m) = self.mixer.lock() {
                m.forget(LOOPBACK);
            }
            if !self.connected {
                self.close_audio();
            }
        }
    }

    pub fn mic_test(&self) -> bool {
        self.mic_test
    }

    /// The microphone's level in dBFS after gain and noise suppression.
    pub fn level_db(&self) -> f32 {
        self.level
    }

    fn close_audio(&mut self) {
        self.capture = None;
        self.playback = None;
    }

    /// Every frame off a server: only the microphone test runs.
    pub fn idle_frame(&mut self) {
        if self.mic_test {
            self.microphone();
        }
    }

    /// The player entered the play state on `host`: announce the channels and
    /// ask for the secret.
    pub fn joined(&mut self, host: &str) {
        self.joined_resolving();
        self.set_host(host);
    }

    /// The player entered the play state; where the server really is (the
    /// voice server's host when it names none) follows with [`Voice::set_host`].
    pub fn joined_resolving(&mut self) {
        self.left();
        self.outgoing.push((payload::REGISTER.into(), payload::register()));
        self.outgoing.push((payload::REQUEST_SECRET.into(), payload::request_secret(self.config.version)));
    }

    /// The host the game's connection reached: a secret waiting for it
    /// connects now.
    pub fn set_host(&mut self, host: &str) {
        self.host = host.to_owned();
        self.host_ready = true;
        if let Some(info) = self.pending.take() {
            self.start(info);
        }
    }

    /// The player left the server (or is reconfigured onto another).
    pub fn left(&mut self) {
        self.host.clear();
        self.host_ready = false;
        self.pending = None;
        self.client = None;
        self.info = None;
        self.connected = false;
        self.status = Status::Idle;
        self.encoder = None;
        self.decoders.clear();
        if !self.mic_test {
            self.close_audio();
        }
        self.sending = false;
        self.wants = false;
        self.groups.clear();
        self.players.clear();
        self.my_group = None;
        if let Ok(mut m) = self.mixer.lock() {
            m.clear();
        }
    }

    /// A plugin message from the server; true when it was voice chat's.
    pub fn payload(&mut self, channel: &str, data: &[u8]) -> bool {
        if !channel.starts_with("voicechat:") {
            return false;
        }
        if self.group_payload(channel, data) {
            return true;
        }
        if channel == payload::SECRET {
            match SecretInfo::decode(data, self.config.version) {
                Ok(info) if self.host_ready => self.start(info),
                Ok(info) => {
                    self.pending = Some(info);
                    self.status = Status::Connecting;
                }
                Err(e) => self.status = Status::Failed(format!("bad secret packet: {e}")),
            }
        }
        true
    }

    fn group_payload(&mut self, channel: &str, data: &[u8]) -> bool {
        let version = self.config.version;
        match channel {
            groups::ADD_GROUP => {
                if let Ok(g) = Group::decode(data) {
                    self.groups.insert(g.id, g);
                }
            }
            groups::REMOVE_GROUP => {
                if let Ok(id) = groups::decode_removed(data) {
                    self.groups.remove(&id);
                }
            }
            groups::JOINED_GROUP => {
                if let Ok((group, wrong_password)) = groups::decode_joined(data) {
                    let l = self.lang;
                    if wrong_password {
                        self.notices.push(l.pick("Wrong group password", "Неверный пароль группы").into());
                    } else if group != self.my_group {
                        let line = match group.and_then(|id| self.groups.get(&id)) {
                            Some(g) => l.pick(format!("You are in group \"{}\"", g.name), format!("Вы в группе «{}»", g.name)),
                            None if group.is_some() => l.pick("You are in a group", "Вы в группе").into(),
                            None => l.pick("You left the group", "Вы вышли из группы").into(),
                        };
                        self.notices.push(line);
                    }
                    self.my_group = group;
                }
            }
            payload::PLAYER_STATE => {
                if let Some(s) = PlayerState::decode_one(data, version) {
                    self.players.insert(s.uuid, s);
                }
            }
            payload::PLAYER_STATES => {
                if let Some(all) = PlayerState::decode_all(data, version) {
                    self.players = all.into_iter().map(|s| (s.uuid, s)).collect();
                }
            }
            _ => return false,
        }
        true
    }

    /// The groups players can see, by name, with how many are in each.
    pub fn group_list(&self) -> Vec<(Group, usize)> {
        let mut list: Vec<(Group, usize)> = self
            .groups
            .values()
            .filter(|g| !g.hidden || Some(g.id) == self.my_group)
            .map(|g| (g.clone(), self.members(g.id).len()))
            .collect();
        list.sort_by(|a, b| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()));
        list
    }

    /// The names of the players in group `id`.
    pub fn members(&self, id: Uuid) -> Vec<String> {
        let mut names: Vec<String> = self
            .players
            .values()
            .filter(|p| p.group == Some(id) && !p.disconnected)
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn current_group(&self) -> Option<&Group> {
        self.my_group.and_then(|id| self.groups.get(&id))
    }

    /// The player's own name, as voice chat knows it.
    pub fn my_name(&self) -> Option<String> {
        let me = self.info.as_ref()?.player;
        self.players.get(&me).map(|p| p.name.clone())
    }

    pub fn find_group(&self, name: &str) -> Option<&Group> {
        let name = name.trim().to_lowercase();
        self.groups.values().find(|g| g.name.to_lowercase() == name)
    }

    pub fn create_group(&mut self, name: &str, password: Option<&str>, kind: GroupType) {
        if self.connected {
            self.outgoing.push((groups::CREATE_GROUP.into(), groups::create(name, password, kind)));
        }
    }

    pub fn join_group(&mut self, id: Uuid, password: Option<&str>) {
        if self.connected {
            self.outgoing.push((groups::JOIN_GROUP.into(), groups::join(id, password)));
        }
    }

    pub fn leave_group(&mut self) {
        if self.connected {
            self.outgoing.push((groups::LEAVE_GROUP.into(), Vec::new()));
        }
    }

    /// Lines for the chat since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    fn start(&mut self, info: SecretInfo) {
        self.client = None;
        match VoiceClient::connect(&info, &self.host) {
            Ok(c) => {
                self.client = Some(c);
                self.status = Status::Connecting;
            }
            Err(e) => self.status = Status::Failed(format!("voice server: {e}")),
        }
        self.encoder = Encoder::new(info.codec).ok();
        self.info = Some(info);
    }

    fn open_audio(&mut self) {
        let mut errors = Vec::new();
        let mic = Some(self.config.mic_device.as_str());
        match Capture::open(mic) {
            Ok(c) => self.capture = Some(c),
            Err(e) => errors.push(format!("microphone: {e}")),
        }
        let speakers = Some(self.config.speaker_device.as_str());
        match Playback::open(speakers, self.mixer.clone()) {
            Ok(p) => self.playback = Some(p),
            Err(e) => errors.push(format!("speakers: {e}")),
        }
        self.audio_error = (!errors.is_empty()).then(|| errors.join("; "));
    }

    /// Plugin messages to send to the server, oldest first.
    pub fn take_outgoing(&mut self) -> Vec<(String, Vec<u8>)> {
        std::mem::take(&mut self.outgoing)
    }

    pub fn set_push_to_talk(&mut self, down: bool) {
        self.ptt = down;
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.send_state();
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    fn send_state(&mut self) {
        if self.connected {
            let (channel, body) = payload::client_state(self.config.version, self.muted, false);
            self.outgoing.push((channel.into(), body));
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn audio_error(&self) -> Option<&str> {
        self.audio_error.as_deref()
    }

    /// The player's own voice is going out (or, in the microphone test, would).
    pub fn talking(&self) -> bool {
        self.sending || (self.mic_test && self.wants)
    }

    /// The players being heard now.
    pub fn speakers(&self) -> Vec<Uuid> {
        self.mixer.lock().map(|m| m.talking()).unwrap_or_default()
    }

    /// Network events in, microphone out. `position_of` finds a player's
    /// position by UUID among the entities the game knows.
    pub fn frame(&mut self, listener: Listener, position_of: &dyn Fn(Uuid) -> Option<[f64; 3]>) {
        let events = self.client.as_ref().map(|c| c.poll()).unwrap_or_default();
        for e in events {
            match e {
                Event::Connected => {
                    self.connected = true;
                    self.status = Status::Connected;
                    if self.capture.is_none() && self.playback.is_none() {
                        self.open_audio();
                    }
                    self.send_state();
                }
                Event::TimedOut => {
                    self.connected = false;
                    self.status = Status::Failed("voice connection timed out".into());
                }
                Event::Sound { sound, location, distance, group, .. } => {
                    self.hear(listener, position_of, sound, location, distance, group);
                }
            }
        }
        self.microphone();
    }

    fn hear(
        &mut self,
        listener: Listener,
        position_of: &dyn Fn(Uuid) -> Option<[f64; 3]>,
        sound: crate::packets::Sound,
        location: Option<[f64; 3]>,
        distance: f32,
        group: bool,
    ) {
        if sound.data.is_empty() {
            // Their end of a sentence: start the next one with a fresh decoder.
            self.decoders.remove(&sound.sender);
            return;
        }
        let pcm: Vec<f32> = if sound.sequence < 0 {
            // Raw s16le from a server-side audio channel.
            sound.data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
        } else {
            let Some(dec) = self.decoder(sound.sender) else { return };
            match dec.decode(&sound.data) {
                Ok(pcm) => pcm.to_vec(),
                Err(_) => return,
            }
        };
        let gains = if group {
            [1.0, 1.0]
        } else {
            match location.or_else(|| position_of(sound.sender)) {
                Some(at) => audio::spatial_gains(listener.position, listener.yaw, at, distance),
                // Not in view: the server only sends voices in range, so play
                // it unplaced rather than drop it.
                None => [0.7, 0.7],
            }
        };
        if let Ok(mut m) = self.mixer.lock() {
            m.push(sound.sender, &pcm, gains);
        }
    }

    fn decoder(&mut self, sender: Uuid) -> Option<&mut Decoder> {
        if !self.decoders.contains_key(&sender) {
            self.decoders.insert(sender, Decoder::new().ok()?);
        }
        self.decoders.get_mut(&sender)
    }

    fn microphone(&mut self) {
        let frames = match &self.capture {
            Some(c) => c.frames(),
            None => return,
        };
        for mut frame in frames {
            if self.config.mic_gain != 1.0 {
                for s in &mut frame {
                    *s = (*s as f32 * self.config.mic_gain).clamp(-32768.0, 32767.0) as i16;
                }
            }
            self.denoiser.process(&mut frame);
            self.level = audio::level_db(&frame);
            if self.level >= self.config.threshold_db {
                self.hangover = HANGOVER_FRAMES;
            } else {
                self.hangover = self.hangover.saturating_sub(1);
            }
            let voice_active = self.hangover > 0;
            self.wants = !self.muted
                && match self.config.activation {
                    Activation::PushToTalk => self.ptt,
                    Activation::Voice => voice_active,
                };
            if self.mic_test {
                // Push-to-talk can't be held in a menu: the test plays it all;
                // with voice activation, what would go out.
                let heard = match self.config.activation {
                    Activation::PushToTalk => true,
                    Activation::Voice => voice_active,
                };
                if heard {
                    let pcm: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();
                    if let Ok(mut m) = self.mixer.lock() {
                        m.push(LOOPBACK, &pcm, [1.0, 1.0]);
                    }
                }
            }
            if !self.connected {
                continue;
            }
            let (Some(client), Some(encoder)) = (&self.client, &mut self.encoder) else { continue };
            if self.wants {
                if let Ok(opus) = encoder.encode(&frame) {
                    client.send_mic(opus, false);
                    self.sending = true;
                }
            } else if self.sending {
                self.sending = false;
                client.end_of_speech();
                // A fresh encoder for the next sentence, as Simple Voice Chat does.
                if let Some(info) = &self.info {
                    if let Ok(fresh) = Encoder::new(info.codec) {
                        *encoder = fresh;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_asks_for_the_secret() {
        let mut v = Voice::new(Config::default());
        v.joined("mc.example");
        let out = v.take_outgoing();
        assert_eq!(out[0].0, payload::REGISTER);
        assert_eq!(out[1], (payload::REQUEST_SECRET.to_owned(), vec![0, 0, 0, 20]));
        assert!(v.take_outgoing().is_empty());
    }

    #[test]
    fn a_secret_waits_for_the_host() {
        let mut v = Voice::new(Config::default());
        v.joined_resolving();
        assert_eq!(v.take_outgoing().len(), 2, "asks for the secret at once");
        let mut w = crate::buf::Writer::new();
        w.bytes(&[1; 16]).i32(24454).uuid(9).u8(0).i32(1024).f64(48.0).i32(1000).bool(false);
        w.string("").bool(false);
        v.payload(payload::SECRET, &w.data);
        assert_eq!(v.status(), &Status::Connecting);
        assert!(v.client.is_none(), "no connection before the host is known");
        v.set_host("127.0.0.1");
        assert!(v.client.is_some());
    }

    #[test]
    fn groups_follow_the_server() {
        use crate::buf::Writer;
        let mut v = Voice::new(Config::default());
        v.lang = Lang::Ru;
        let mut g = Writer::new();
        g.uuid(7).string("Base").bool(false).bool(false).bool(false).i16(0);
        v.payload(groups::ADD_GROUP, &g.data);
        let mut states = Writer::new();
        states.i32(2);
        states.bool(false).bool(false).uuid(1).string("Steve").bool(true).uuid(7);
        states.bool(false).bool(false).uuid(2).string("Alex").bool(false);
        v.payload(payload::PLAYER_STATES, &states.data);
        assert_eq!(v.group_list()[0].1, 1);
        assert_eq!(v.members(7), ["Steve"]);
        assert_eq!(v.find_group("base").unwrap().id, 7);

        let mut j = Writer::new();
        j.bool(true).uuid(7).bool(false);
        v.payload(groups::JOINED_GROUP, &j.data);
        assert_eq!(v.current_group().unwrap().name, "Base");
        assert_eq!(v.take_notices(), ["Вы в группе «Base»"]);
        v.lang = Lang::En;
        let mut left = Writer::new();
        left.bool(false).bool(false);
        v.payload(groups::JOINED_GROUP, &left.data);
        assert_eq!(v.take_notices(), ["You left the group"]);
        v.lang = Lang::Ru;
        v.payload(groups::JOINED_GROUP, &[0, 1]);
        assert_eq!(v.take_notices(), ["Неверный пароль группы"]);
        assert!(v.current_group().is_none());

        let mut r = Writer::new();
        r.uuid(7);
        v.payload(groups::REMOVE_GROUP, &r.data);
        assert!(v.group_list().is_empty());
    }

    #[test]
    fn only_voicechat_payloads_are_taken() {
        let mut v = Voice::new(Config::default());
        assert!(!v.payload("minecraft:brand", b"x"));
        assert!(v.payload("voicechat:states", b""));
        assert!(v.payload(payload::SECRET, b"short"));
        assert!(matches!(v.status(), Status::Failed(_)));
    }

    #[test]
    fn config_round_trips_and_tolerates_junk() {
        let mut c = Config::default();
        c.version = Version::V21;
        c.activation = Activation::Voice;
        c.ptt_key = "key.keyboard.caps.lock".into();
        c.volume = 0.5;
        c.noise = Strength::High;
        assert_eq!(Config::parse(&c.to_text()), c);
        let junk = Config::parse("volume=loud\nnonsense\nactivation=maybe # x\ncompatibility_version=99");
        assert_eq!(junk, Config::default());
    }
}
