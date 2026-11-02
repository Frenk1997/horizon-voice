//! The voice connection's state machine, without a socket: feed it received
//! datagrams and the clock, send what it returns.
//!
//! Authenticate once a second until the server acknowledges, then send the
//! connection check with the server's nonce once a second until that is
//! acknowledged. Answer every keep-alive and ping; with nothing heard for ten
//! keep-alive intervals the connection has timed out.

use std::time::{Duration, Instant};

use crate::Version;
use crate::buf::Uuid;
use crate::crypto::Secret;
use crate::packets::{self, NONCE_LEN, Packet, Sound};
use crate::payload::SecretInfo;

const RETRY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Authenticating,
    Checking { nonce: Option<[u8; NONCE_LEN]> },
    Connected,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connected,
    TimedOut,
    /// Another player's voice. `location` is set for a sound at a fixed
    /// place; `distance` is the range it carries (0 for group sound).
    Sound { sound: Sound, location: Option<[f64; 3]>, distance: f32, whispering: bool, group: bool },
}

pub struct Session {
    version: Version,
    secret: Secret,
    /// The id in each datagram's header: the connection id, or in version
    /// 20 the player's UUID.
    connection: Uuid,
    player: Uuid,
    keep_alive: Duration,
    state: State,
    next_attempt: Instant,
    last_heard: Instant,
    sequence: i64,
}

impl Session {
    pub fn new(info: &SecretInfo, now: Instant) -> Self {
        Session {
            version: info.version,
            secret: Secret::new(info.secret),
            connection: info.header_id(),
            player: info.player,
            keep_alive: Duration::from_millis(info.keep_alive.max(1) as u64),
            state: State::Authenticating,
            next_attempt: now,
            last_heard: now,
            sequence: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_connected(&self) -> bool {
        self.state == State::Connected
    }

    fn datagram(&self, packet: &Packet) -> Vec<u8> {
        packets::write_client(&self.secret, self.connection, packet).expect("encrypting a voice packet")
    }

    fn authenticate(&self) -> Packet {
        Packet::Authenticate {
            credentials: match self.version {
                Version::V20 => Some((self.player, *self.secret.bytes())),
                Version::V21 => None,
            },
        }
    }

    /// What to send now: handshake retries, and the timeout check.
    pub fn tick(&mut self, now: Instant, events: &mut Vec<Event>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        match self.state {
            State::Authenticating | State::Checking { .. } if now >= self.next_attempt => {
                let packet = match self.state {
                    State::Checking { nonce } => Packet::ConnectionCheck { nonce },
                    _ => self.authenticate(),
                };
                out.push(self.datagram(&packet));
                self.next_attempt = now + RETRY;
            }
            State::Connected if now.duration_since(self.last_heard) > self.keep_alive * 10 => {
                self.state = State::TimedOut;
                events.push(Event::TimedOut);
            }
            _ => {}
        }
        out
    }

    /// A datagram from the server; returns the replies to send.
    pub fn receive(&mut self, datagram: &[u8], now: Instant, events: &mut Vec<Event>) -> Vec<Vec<u8>> {
        let Ok(packet) = packets::read_client(&self.secret, datagram, self.version) else {
            return Vec::new();
        };
        if self.state == State::TimedOut {
            return Vec::new();
        }
        self.last_heard = now;
        let mut out = Vec::new();
        match packet {
            Packet::AuthenticateAck { nonce } if self.state == State::Authenticating => {
                self.state = State::Checking { nonce };
                // Check at once rather than a second later.
                out.push(self.datagram(&Packet::ConnectionCheck { nonce }));
                self.next_attempt = now + RETRY;
            }
            Packet::ConnectionCheckAck if matches!(self.state, State::Checking { .. }) => {
                self.state = State::Connected;
                events.push(Event::Connected);
            }
            Packet::KeepAlive => out.push(self.datagram(&Packet::KeepAlive)),
            ping @ Packet::Ping { .. } => out.push(self.datagram(&ping)),
            Packet::PlayerSound { sound, distance, whispering } => {
                events.push(Event::Sound { sound, location: None, distance, whispering, group: false });
            }
            Packet::LocationSound { sound, location, distance } => {
                events.push(Event::Sound { sound, location: Some(location), distance, whispering: false, group: false });
            }
            Packet::GroupSound { sound } => {
                events.push(Event::Sound { sound, location: None, distance: 0.0, whispering: false, group: true });
            }
            _ => {}
        }
        out
    }

    /// One encoded Opus frame from the microphone; `None` until connected.
    pub fn mic(&mut self, opus: &[u8], whispering: bool) -> Option<Vec<u8>> {
        if !self.is_connected() {
            return None;
        }
        let packet = Packet::Mic { data: opus.to_vec(), sequence: self.sequence, whispering };
        self.sequence += 1;
        Some(self.datagram(&packet))
    }

    /// The end of a sentence: an empty microphone packet, which makes the
    /// listeners' clients play out what they buffered. The sequence goes on
    /// counting: they put packets in order by it for the whole session.
    pub fn stop(&mut self) -> Option<Vec<u8>> {
        self.mic(&[], false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buf::{Reader, Writer};
    use crate::payload::Codec;

    const PLAYER: Uuid = 0x77;

    const KEY: [u8; 16] = [0x42; 16];
    const CONN: Uuid = 0x1234;

    fn info(version: Version) -> SecretInfo {
        SecretInfo {
            version,
            secret: KEY,
            connection_id: (version == Version::V21).then_some(CONN),
            server_port: 24454,
            player: PLAYER,
            codec: Codec::Voip,
            mtu: 1024,
            distance: 48.0,
            keep_alive: 1000,
            groups: false,
            voice_host: String::new(),
            allow_recording: false,
        }
    }

    /// What the server would decode from a client datagram.
    fn server_reads_as(d: &[u8], version: Version) -> Packet {
        let mut r = Reader::new(d);
        assert_eq!(r.u8().unwrap(), packets::MAGIC);
        let header = if version == Version::V21 { CONN } else { PLAYER };
        assert_eq!(r.uuid().unwrap(), header);
        let enc = r.byte_array(packets::MAX_PACKET).unwrap();
        Packet::decode(&Secret::new(KEY).decrypt(&enc).unwrap(), version).unwrap()
    }

    fn server_reads(d: &[u8]) -> Packet {
        server_reads_as(d, Version::V21)
    }

    fn server_sends(p: &Packet) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(packets::MAGIC).byte_array(&Secret::new(KEY).encrypt(&p.encode()).unwrap());
        w.into_inner()
    }

    #[test]
    fn handshake_then_voice() {
        let t0 = Instant::now();
        let mut ev = Vec::new();
        let mut s = Session::new(&info(Version::V21), t0);
        let auth = Packet::Authenticate { credentials: None };

        let out = s.tick(t0, &mut ev);
        assert_eq!(server_reads(&out[0]), auth);
        assert!(s.tick(t0 + Duration::from_millis(500), &mut ev).is_empty(), "retries once a second");
        assert_eq!(server_reads(&s.tick(t0 + RETRY, &mut ev)[0]), auth);
        assert!(s.mic(&[1], false).is_none());

        let nonce = Some([9; 16]);
        let out = s.receive(&server_sends(&Packet::AuthenticateAck { nonce }), t0, &mut ev);
        assert_eq!(server_reads(&out[0]), Packet::ConnectionCheck { nonce });

        s.receive(&server_sends(&Packet::ConnectionCheckAck), t0, &mut ev);
        assert_eq!(ev, [Event::Connected]);
        assert!(s.tick(t0 + RETRY * 5, &mut ev).is_empty(), "no handshake once connected");

        let a = server_reads(&s.mic(&[7, 7], true).unwrap());
        let b = server_reads(&s.mic(&[8], false).unwrap());
        assert_eq!(a, Packet::Mic { data: vec![7, 7], sequence: 0, whispering: true });
        assert_eq!(b, Packet::Mic { data: vec![8], sequence: 1, whispering: false });
        // Releasing push-to-talk sends an empty packet, and the next sentence
        // carries on from it rather than starting over.
        let stop = server_reads(&s.stop().unwrap());
        assert_eq!(stop, Packet::Mic { data: vec![], sequence: 2, whispering: false });
        let c = server_reads(&s.mic(&[9], false).unwrap());
        assert_eq!(c, Packet::Mic { data: vec![9], sequence: 3, whispering: false });
    }

    #[test]
    fn answers_keep_alive_and_ping_and_times_out() {
        let t0 = Instant::now();
        let mut ev = Vec::new();
        let mut s = Session::new(&info(Version::V21), t0);
        s.receive(&server_sends(&Packet::AuthenticateAck { nonce: Some([0; 16]) }), t0, &mut ev);
        s.receive(&server_sends(&Packet::ConnectionCheckAck), t0, &mut ev);

        let out = s.receive(&server_sends(&Packet::KeepAlive), t0, &mut ev);
        assert_eq!(server_reads(&out[0]), Packet::KeepAlive);
        let ping = Packet::Ping { id: 5, timestamp: 6 };
        assert_eq!(server_reads(&s.receive(&server_sends(&ping), t0, &mut ev)[0]), ping);

        ev.clear();
        s.tick(t0 + Duration::from_secs(9), &mut ev);
        assert!(ev.is_empty());
        s.tick(t0 + Duration::from_secs(11), &mut ev);
        assert_eq!(ev, [Event::TimedOut]);
        assert_eq!(s.state(), State::TimedOut);
    }

    #[test]
    fn delivers_sound_and_ignores_garbage() {
        let t0 = Instant::now();
        let mut ev = Vec::new();
        let mut s = Session::new(&info(Version::V21), t0);
        assert!(s.receive(&[0xFF, 3, 1, 2, 3], t0, &mut ev).is_empty());
        let sound = Sound { channel: 1, sender: 2, data: vec![1], sequence: 0, category: None };
        s.receive(
            &server_sends(&Packet::LocationSound { sound: sound.clone(), location: [1.0, 2.0, 3.0], distance: 16.0 }),
            t0,
            &mut ev,
        );
        assert_eq!(
            ev,
            [Event::Sound { sound, location: Some([1.0, 2.0, 3.0]), distance: 16.0, whispering: false, group: false }]
        );
    }

    #[test]
    fn v20_handshake() {
        let t0 = Instant::now();
        let mut ev = Vec::new();
        let mut s = Session::new(&info(Version::V20), t0);
        let out = s.tick(t0, &mut ev);
        assert_eq!(
            server_reads_as(&out[0], Version::V20),
            Packet::Authenticate { credentials: Some((PLAYER, KEY)) }
        );
        let out = s.receive(&server_sends(&Packet::AuthenticateAck { nonce: None }), t0, &mut ev);
        assert_eq!(server_reads_as(&out[0], Version::V20), Packet::ConnectionCheck { nonce: None });
        s.receive(&server_sends(&Packet::ConnectionCheckAck), t0, &mut ev);
        assert_eq!(ev, [Event::Connected]);
    }
}
