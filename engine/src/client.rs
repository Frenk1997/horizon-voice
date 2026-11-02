//! The voice connection on its own thread: a UDP socket driving a
//! [`Session`]. The game hands it the secret and microphone frames and takes
//! events back, never blocking on the network.

use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::packets::MAX_PACKET;
use crate::payload::SecretInfo;
use crate::session::{Event, Session};

enum Command {
    Mic { opus: Vec<u8>, whispering: bool },
    EndOfSpeech,
    Stop,
}

pub struct VoiceClient {
    commands: Sender<Command>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl VoiceClient {
    /// Opens the socket to the voice server named by `info` (falling back to
    /// `minecraft_host`) and starts the handshake.
    pub fn connect(info: &SecretInfo, minecraft_host: &str) -> std::io::Result<VoiceClient> {
        let (host, port) = info.voice_address(minecraft_host);
        let mut addrs: Vec<_> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| std::io::Error::other(format!("{host}:{port}: {e}")))?
            .collect();
        // IPv4 first, as the game connects.
        addrs.sort_by_key(|a| a.is_ipv6());
        let server = *addrs.first().ok_or_else(|| std::io::Error::other(format!("no address for {host}")))?;
        let bind = if server.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let socket = UdpSocket::bind(bind)?;
        socket.connect(server)?;
        socket.set_read_timeout(Some(Duration::from_millis(10)))?;

        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let session = Session::new(info, Instant::now());
        let thread = thread::Builder::new()
            .name("horizon-voice-net".into())
            .spawn(move || run(socket, session, command_rx, event_tx))?;
        Ok(VoiceClient { commands, events, thread: Some(thread) })
    }

    pub fn send_mic(&self, opus: Vec<u8>, whispering: bool) {
        let _ = self.commands.send(Command::Mic { opus, whispering });
    }

    pub fn end_of_speech(&self) {
        let _ = self.commands.send(Command::EndOfSpeech);
    }

    /// Events since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for VoiceClient {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(socket: UdpSocket, mut session: Session, commands: Receiver<Command>, events: Sender<Event>) {
    let mut buf = vec![0u8; MAX_PACKET + 64];
    let mut pending = Vec::new();
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Mic { opus, whispering }) => {
                    if let Some(d) = session.mic(&opus, whispering) {
                        let _ = socket.send(&d);
                    }
                }
                Ok(Command::EndOfSpeech) => {
                    if let Some(d) = session.stop() {
                        let _ = socket.send(&d);
                    }
                }
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        let now = Instant::now();
        let mut out = session.tick(now, &mut pending);
        match socket.recv(&mut buf) {
            Ok(n) => out.extend(session.receive(&buf[..n], now, &mut pending)),
            // A timeout, or Windows' ICMP "port unreachable" while the server
            // isn't listening yet: keep retrying.
            Err(_) => {}
        }
        for d in out {
            let _ = socket.send(&d);
        }
        for e in pending.drain(..) {
            if events.send(e).is_err() {
                return;
            }
        }
    }
}
