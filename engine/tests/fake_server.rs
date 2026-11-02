//! The client over real UDP against a stand-in server on localhost that
//! answers as Simple Voice Chat's does.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use horizon_voice_engine::buf::{Reader, Writer};
use horizon_voice_engine::client::VoiceClient;
use horizon_voice_engine::crypto::Secret;
use horizon_voice_engine::packets::{MAGIC, MAX_PACKET, Packet, Sound};
use horizon_voice_engine::payload::{Codec, SecretInfo};
use horizon_voice_engine::session::Event;
use horizon_voice_engine::Version;

const KEY: [u8; 16] = *b"0123456789abcdef";
/// Version 20 puts the player's UUID in each datagram's header.
const PLAYER: u128 = 0xC0FFEE;

fn decode(d: &[u8]) -> (u128, Packet) {
    let mut r = Reader::new(d);
    assert_eq!(r.u8().unwrap(), MAGIC);
    let conn = r.uuid().unwrap();
    let enc = r.byte_array(MAX_PACKET).unwrap();
    (conn, Packet::decode(&Secret::new(KEY).decrypt(&enc).unwrap(), Version::V20).unwrap())
}

fn encode(p: &Packet) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(MAGIC).byte_array(&Secret::new(KEY).encrypt(&p.encode()).unwrap());
    w.into_inner()
}

fn wait_for(client: &VoiceClient, want: impl Fn(&Event) -> bool) -> Event {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(e) = client.poll().into_iter().find(|e| want(e)) {
            return e;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("event did not arrive");
}

#[test]
fn handshake_mic_and_sound_over_udp() {
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let info = SecretInfo {
        version: Version::V20,
        secret: KEY,
        connection_id: None,
        server_port: server.local_addr().unwrap().port() as i32,
        player: PLAYER,
        codec: Codec::Voip,
        mtu: 1024,
        distance: 48.0,
        keep_alive: 1000,
        groups: false,
        voice_host: String::new(),
        allow_recording: false,
    };
    let client = VoiceClient::connect(&info, "127.0.0.1").unwrap();
    let mut buf = [0u8; 8192];

    let (n, from) = server.recv_from(&mut buf).unwrap();
    let auth = Packet::Authenticate { credentials: Some((PLAYER, KEY)) };
    assert_eq!(decode(&buf[..n]), (PLAYER, auth));
    server.send_to(&encode(&Packet::AuthenticateAck { nonce: None }), from).unwrap();

    // Skip any Authenticate retries still in flight.
    loop {
        let (n, _) = server.recv_from(&mut buf).unwrap();
        match decode(&buf[..n]).1 {
            Packet::Authenticate { .. } => continue,
            p => {
                assert_eq!(p, Packet::ConnectionCheck { nonce: None });
                break;
            }
        }
    }
    server.send_to(&encode(&Packet::ConnectionCheckAck), from).unwrap();
    wait_for(&client, |e| *e == Event::Connected);

    client.send_mic(vec![1, 2, 3], false);
    let (n, _) = server.recv_from(&mut buf).unwrap();
    assert_eq!(decode(&buf[..n]).1, Packet::Mic { data: vec![1, 2, 3], sequence: 0, whispering: false });

    server.send_to(&encode(&Packet::KeepAlive), from).unwrap();
    let (n, _) = server.recv_from(&mut buf).unwrap();
    assert_eq!(decode(&buf[..n]).1, Packet::KeepAlive);

    let sound = Sound { channel: 3, sender: 4, data: vec![9], sequence: 0, category: None };
    server
        .send_to(&encode(&Packet::PlayerSound { sound: sound.clone(), distance: 48.0, whispering: false }), from)
        .unwrap();
    let e = wait_for(&client, |e| matches!(e, Event::Sound { .. }));
    assert_eq!(e, Event::Sound { sound, location: None, distance: 48.0, whispering: false, group: false });
}
