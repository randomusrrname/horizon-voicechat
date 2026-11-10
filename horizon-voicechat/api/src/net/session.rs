//! A UDP client that follows the sequence seen in captures:
//!
//! 1. send `Authenticate`, get `AuthenticateAck`
//! 2. about one keep-alive interval later send `ConnectionCheck`, get
//!    `ConnectionCheckAck`; from then on the session is connected and voice
//!    may be sent
//! 3. keep-alives in both directions every keep-alive interval, also while
//!    voice is flowing
//!
//! The client is non-blocking and has no thread of its own: call
//! [`UdpClient::poll`] once per tick (or from a thread you own).

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::tcp::Secret;
use super::udp::{self, Cipher, Packet};
use super::Uuid;

/// How often the authentication is repeated until the server answers.
/// (Our choice: captures only show one try.)
const AUTH_RESEND: Duration = Duration::from_secs(1);
/// The session is reported dead after this many silent keep-alive
/// intervals. (Our choice.)
const IDLE_TIMEOUT_INTERVALS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Authenticating,
    Checking,
    Connected,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The handshake finished: voice can be sent.
    Connected,
    /// Nothing authentic arrived for a long time. Reported once; the
    /// session recovers by itself if the server answers again.
    TimedOut,
    /// Any packet that is not part of the handshake or keep-alive, for
    /// example other players' voice once that format is implemented.
    Received(Packet),
}

pub struct UdpClient {
    socket: UdpSocket,
    cipher: Cipher,
    player: Uuid,
    secret: Uuid,
    keep_alive: Duration,
    state: State,
    sequence: i64,
    next_action: Instant,
    last_rx: Instant,
    timed_out: bool,
}

impl UdpClient {
    /// Opens a socket towards `server` and sends the first `Authenticate`.
    pub fn new(server: SocketAddr, secret: &Secret, now: Instant) -> io::Result<UdpClient> {
        let bind: SocketAddr = if server.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            ([0u16; 8], 0).into()
        };
        let socket = UdpSocket::bind(bind)?;
        socket.connect(server)?;
        socket.set_nonblocking(true)?;

        let mut client = UdpClient {
            socket,
            cipher: Cipher::new(&secret.secret),
            player: secret.player,
            secret: secret.secret,
            keep_alive: Duration::from_millis(secret.keep_alive_ms.max(1) as u64),
            state: State::Authenticating,
            sequence: 0,
            next_action: now + AUTH_RESEND,
            last_rx: now,
            timed_out: false,
        };
        client.send_authenticate(now)?;
        Ok(client)
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_connected(&self) -> bool {
        self.state == State::Connected
    }

    /// Sends the empty frame that ends a transmission (call it when the
    /// microphone goes quiet or the push-to-talk key is released). The
    /// sequence number is never reset: it counts on across transmissions.
    pub fn send_mic_end(&mut self) -> io::Result<bool> {
        self.send_mic(&[], 0)
    }

    /// Sends one Opus frame. Returns `Ok(false)` without sending anything
    /// while the handshake is not finished.
    pub fn send_mic(&mut self, opus: &[u8], flags: u8) -> io::Result<bool> {
        if self.state != State::Connected {
            return Ok(false);
        }
        let packet = Packet::Mic { opus: opus.to_vec(), sequence: self.sequence, flags };
        self.send(&packet)?;
        self.sequence += 1;
        Ok(true)
    }

    /// Reads everything waiting on the socket, advances the handshake and
    /// the timers, and returns what the host should know about.
    pub fn poll(&mut self, now: Instant) -> io::Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            match self.socket.recv(&mut buf) {
                Ok(n) => self.handle(&buf[..n], now, &mut events),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // Windows reports an earlier ICMP "port unreachable" this way.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => break,
                Err(e) => return Err(e),
            }
        }
        self.run_timers(now, &mut events)?;
        Ok(events)
    }

    fn handle(&mut self, datagram: &[u8], now: Instant, events: &mut Vec<Event>) {
        // Anything that does not decrypt with our secret is dropped.
        let Ok(payload) = udp::unframe_from_server(datagram) else { return };
        let Ok(plain) = self.cipher.open(payload) else { return };
        let Ok(packet) = Packet::decode(&plain) else { return };

        self.last_rx = now;
        self.timed_out = false;

        match (self.state, &packet) {
            (State::Authenticating, Packet::AuthenticateAck) => {
                self.state = State::Checking;
                self.next_action = now + self.keep_alive;
            }
            (State::Checking, Packet::ConnectionCheckAck) => {
                self.state = State::Connected;
                self.next_action = now + self.keep_alive;
                events.push(Event::Connected);
            }
            (_, Packet::KeepAlive) => {}
            _ => events.push(Event::Received(packet)),
        }
    }

    fn run_timers(&mut self, now: Instant, events: &mut Vec<Event>) -> io::Result<()> {
        if now >= self.next_action {
            match self.state {
                State::Authenticating => {
                    self.send_authenticate(now)?;
                    self.next_action = now + AUTH_RESEND;
                }
                State::Checking => {
                    self.send(&Packet::ConnectionCheck)?;
                    self.next_action = now + self.keep_alive;
                }
                State::Connected => {
                    self.send(&Packet::KeepAlive)?;
                    self.next_action = now + self.keep_alive;
                }
            }
        }
        let silent = now.saturating_duration_since(self.last_rx);
        if !self.timed_out && silent > self.keep_alive * IDLE_TIMEOUT_INTERVALS {
            self.timed_out = true;
            events.push(Event::TimedOut);
        }
        Ok(())
    }

    fn send_authenticate(&mut self, _now: Instant) -> io::Result<()> {
        let packet = Packet::Authenticate { player: self.player, secret: self.secret };
        self.send(&packet)
    }

    fn send(&mut self, packet: &Packet) -> io::Result<()> {
        let payload = self.cipher.seal(&packet.encode());
        let datagram = udp::frame_to_server(&self.player, &payload);
        self.socket.send(&datagram)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn fake_secret() -> Secret {
        Secret {
            secret: Uuid([0x11; 16]),
            server_port: 0,
            player: Uuid([0x22; 16]),
            codec: 0,
            mtu_size: 1275,
            voice_distance: 48.0,
            keep_alive_ms: 1000,
            groups_enabled: true,
            voice_host: String::new(),
            allow_recording: true,
        }
    }

    /// Reads one datagram on the fake server and decodes it.
    fn server_recv(server: &UdpSocket, cipher: &Cipher) -> (SocketAddr, Uuid, Packet) {
        let mut buf = [0u8; 2048];
        let (n, from) = server.recv_from(&mut buf).expect("client sent nothing");
        let (player, payload) = udp::unframe_to_server(&buf[..n]).unwrap();
        let plain = cipher.open(payload).unwrap();
        (from, player, Packet::decode(&plain).unwrap())
    }

    /// Sends a packet padded to 256 bytes, as the real server does.
    fn server_send(server: &UdpSocket, cipher: &Cipher, to: SocketAddr, packet: &Packet) {
        let mut plain = packet.encode();
        plain.resize(256, 0);
        let datagram = udp::frame_from_server(&cipher.seal(&plain));
        server.send_to(&datagram, to).unwrap();
    }

    fn poll_until(client: &mut UdpClient, now: Instant, want: impl Fn(&UdpClient) -> bool) -> Vec<Event> {
        let mut all = Vec::new();
        for _ in 0..200 {
            all.extend(client.poll(now).unwrap());
            if want(client) {
                return all;
            }
            sleep(Duration::from_millis(5));
        }
        panic!("state never reached, stuck in {:?}", client.state());
    }

    fn poll_until_event(client: &mut UdpClient, now: Instant) -> Vec<Event> {
        for _ in 0..200 {
            let events = client.poll(now).unwrap();
            if !events.is_empty() {
                return events;
            }
            sleep(Duration::from_millis(5));
        }
        panic!("no event arrived");
    }

    #[test]
    fn full_handshake_then_voice_against_a_fake_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let secret = fake_secret();
        let cipher = Cipher::new(&secret.secret);
        let t0 = Instant::now();
        let mut client = UdpClient::new(server.local_addr().unwrap(), &secret, t0).unwrap();

        // 1. Authenticate
        let (from, player, packet) = server_recv(&server, &cipher);
        assert_eq!(player, secret.player);
        assert_eq!(packet, Packet::Authenticate { player: secret.player, secret: secret.secret });
        server_send(&server, &cipher, from, &Packet::AuthenticateAck);
        poll_until(&mut client, t0, |c| c.state() == State::Checking);

        // Nothing is sent before a keep-alive interval has passed...
        client.poll(t0 + Duration::from_millis(500)).unwrap();
        server.set_nonblocking(true).unwrap();
        let mut scratch = [0u8; 2048];
        assert!(server.recv_from(&mut scratch).is_err());
        server.set_nonblocking(false).unwrap();

        // 2. ...then the connection check goes out.
        client.poll(t0 + Duration::from_millis(1100)).unwrap();
        let (_, _, packet) = server_recv(&server, &cipher);
        assert_eq!(packet, Packet::ConnectionCheck);
        server_send(&server, &cipher, from, &Packet::ConnectionCheckAck);
        let events = poll_until(&mut client, t0 + Duration::from_millis(1100), |c| c.is_connected());
        assert!(events.contains(&Event::Connected));

        // 3. Voice flows, with consecutive sequence numbers.
        assert!(client.send_mic(&[1, 2, 3], 0).unwrap());
        assert!(client.send_mic(&[4, 5], 0).unwrap());
        let (_, _, first) = server_recv(&server, &cipher);
        let (_, _, second) = server_recv(&server, &cipher);
        assert_eq!(first, Packet::Mic { opus: vec![1, 2, 3], sequence: 0, flags: 0 });
        assert_eq!(second, Packet::Mic { opus: vec![4, 5], sequence: 1, flags: 0 });

        // The end of a transmission is an empty frame and the sequence goes on.
        assert!(client.send_mic_end().unwrap());
        let (_, _, end) = server_recv(&server, &cipher);
        assert_eq!(end, Packet::Mic { opus: vec![], sequence: 2, flags: 0 });

        // 4. Incoming voice reaches the host as an event.
        let heard = Packet::PlayerSound {
            first_id: Uuid([3; 16]),
            second_id: Uuid([3; 16]),
            opus: vec![1, 2],
            sequence: 5,
            distance: 48.0,
        };
        server_send(&server, &cipher, from, &heard);
        let events = poll_until_event(&mut client, t0 + Duration::from_millis(1100));
        assert!(events.contains(&Event::Received(heard)));

        // 5. Keep-alive after another interval.
        client.poll(t0 + Duration::from_millis(2200)).unwrap();
        let (_, _, packet) = server_recv(&server, &cipher);
        assert_eq!(packet, Packet::KeepAlive);
    }

    #[test]
    fn voice_is_not_sent_before_the_handshake_ends() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut client =
            UdpClient::new(server.local_addr().unwrap(), &fake_secret(), Instant::now()).unwrap();
        assert!(!client.send_mic(&[1, 2, 3], 0).unwrap());
    }

    #[test]
    fn datagrams_with_the_wrong_key_are_ignored() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let secret = fake_secret();
        let t0 = Instant::now();
        let mut client = UdpClient::new(server.local_addr().unwrap(), &secret, t0).unwrap();
        let (from, _, _) = server_recv(&server, &Cipher::new(&secret.secret));

        let wrong = Cipher::new(&Uuid([0x99; 16]));
        server_send(&server, &wrong, from, &Packet::AuthenticateAck);
        sleep(Duration::from_millis(50));
        client.poll(t0).unwrap();
        assert_eq!(client.state(), State::Authenticating);
    }
}
