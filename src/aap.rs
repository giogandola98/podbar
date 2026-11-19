//! Apple accessory protocol (AAP) over L2CAP PSM 0x1001: the channel AirPods
//! use for battery, noise control, ear detection and conversational
//! awareness. Packet layouts follow LibrePods
//! (https://github.com/kavishdevar/librepods, GPL-3.0).
//!
//! One blocking thread per link reads packets (SOCK_SEQPACKET keeps them
//! whole) and forwards them to the async side; writes happen from the
//! caller.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

const PSM: u16 = 0x1001;
const BTPROTO_L2CAP: i32 = 0;

const HANDSHAKE: &[u8] = &[0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 0];
const SET_FEATURES: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0xd7, 0x00, 0, 0, 0, 0, 0, 0];
const REQUEST_NOTIFICATIONS: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff];

const CONTROL: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x09, 0x00];
const HANDSHAKE_ACK: &[u8] = &[0x01, 0x00, 0x04, 0x00];
const FEATURES_ACK: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x2b, 0x00];
const BATTERY: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x04, 0x00];
const EAR: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x06, 0x00];
const METADATA: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x1d];
const CA_DATA: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x02, 0x00, 0x01];

const ID_NOISE: u8 = 0x0d;
const ID_CA: u8 = 0x28;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Noise {
    Off,
    Anc,
    Transparency,
    Adaptive,
}

impl Noise {
    /// Menu order.
    pub const ALL: [Noise; 4] = [Noise::Off, Noise::Transparency, Noise::Adaptive, Noise::Anc];

    fn wire(self) -> u8 {
        match self {
            Noise::Off => 1,
            Noise::Anc => 2,
            Noise::Transparency => 3,
            Noise::Adaptive => 4,
        }
    }

    fn from_wire(b: u8) -> Option<Noise> {
        Some(match b {
            1 => Noise::Off,
            2 => Noise::Anc,
            3 => Noise::Transparency,
            4 => Noise::Adaptive,
            _ => return None,
        })
    }

    pub fn parse(s: &str) -> Option<Noise> {
        Some(match s {
            "off" => Noise::Off,
            "anc" | "nc" | "noise-cancellation" => Noise::Anc,
            "transparency" | "t" => Noise::Transparency,
            "adaptive" | "a" => Noise::Adaptive,
            _ => return None,
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Noise::Off => "off",
            Noise::Anc => "anc",
            Noise::Transparency => "transparency",
            Noise::Adaptive => "adaptive",
        }
    }
}

/// Where a bud is, as reported by ear detection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    InEar,
    Out,
    InCase,
    #[default]
    Unknown,
}

impl Place {
    fn from_wire(b: u8) -> Place {
        match b {
            0 => Place::InEar,
            1 => Place::Out,
            2 => Place::InCase,
            _ => Place::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Headset,
    Right,
    Left,
    Case,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, PartialEq)]
pub enum Msg {
    HandshakeAck,
    FeaturesAck,
    /// `None` level: the part is not reachable right now (bud in a closed case)
    Battery(Vec<(Part, Option<Level>)>),
    Noise(Noise),
    Ear(Place, Place),
    Ca(bool),
    /// conversational awareness: the wearer started (1) or stopped speaking
    CaData(u8),
    Metadata { name: String, model: String },
}

pub fn control(id: u8, value: u8) -> Vec<u8> {
    let mut p = CONTROL.to_vec();
    p.extend_from_slice(&[id, value, 0, 0, 0]);
    p
}

pub fn noise_packet(n: Noise) -> Vec<u8> {
    control(ID_NOISE, n.wire())
}

pub fn ca_packet(on: bool) -> Vec<u8> {
    control(ID_CA, if on { 1 } else { 2 })
}

pub fn parse(d: &[u8]) -> Option<Msg> {
    if d.starts_with(HANDSHAKE_ACK) {
        return Some(Msg::HandshakeAck);
    }
    if d.starts_with(FEATURES_ACK) {
        return Some(Msg::FeaturesAck);
    }
    if d.starts_with(CONTROL) && d.len() >= 8 {
        return match d[6] {
            ID_NOISE if d.len() == 11 => Noise::from_wire(d[7]).map(Msg::Noise),
            ID_CA => match d[7] {
                1 => Some(Msg::Ca(true)),
                2 => Some(Msg::Ca(false)),
                _ => None,
            },
            _ => None,
        };
    }
    if d.starts_with(EAR) && d.len() == 8 {
        return Some(Msg::Ear(Place::from_wire(d[6]), Place::from_wire(d[7])));
    }
    if d.starts_with(BATTERY) && d.len() >= 7 {
        let n = d[6] as usize;
        if n > 4 || d.len() != 7 + 5 * n {
            return None;
        }
        let mut parts = Vec::with_capacity(n);
        for c in d[7..].chunks_exact(5) {
            let part = match c[0] {
                0x01 => Part::Headset,
                0x02 => Part::Right,
                0x04 => Part::Left,
                0x08 => Part::Case,
                _ => continue,
            };
            let level = match c[3] {
                0x01 => Some(Level { percent: c[2].min(100), charging: true }),
                0x02 => Some(Level { percent: c[2].min(100), charging: false }),
                _ => None,
            };
            parts.push((part, level));
        }
        return Some(Msg::Battery(parts));
    }
    if d.starts_with(CA_DATA) && d.len() == 10 {
        return Some(Msg::CaData(d[9]));
    }
    if d.starts_with(METADATA) && d.len() > METADATA.len() + 6 {
        let mut fields = d[METADATA.len() + 6..].split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
        let name = fields.next().unwrap_or_default();
        let model = fields.next().unwrap_or_default();
        return Some(Msg::Metadata { name, model });
    }
    None
}

pub fn model_name(model: &str) -> Option<&'static str> {
    Some(match model {
        "A1523" | "A1722" => "AirPods",
        "A2032" | "A2031" => "AirPods (2nd gen)",
        "A2565" | "A2564" => "AirPods (3rd gen)",
        "A3053" | "A3050" | "A3054" => "AirPods 4",
        "A3056" | "A3055" | "A3057" => "AirPods 4 ANC",
        "A2084" | "A2083" => "AirPods Pro",
        "A2931" | "A2699" | "A2698" => "AirPods Pro 2",
        "A3047" | "A3048" | "A3049" => "AirPods Pro 2 (USB-C)",
        "A2096" => "AirPods Max",
        "A3184" => "AirPods Max (USB-C)",
        _ => return None,
    })
}

/// An open AAP channel.
#[derive(Clone)]
pub struct Link {
    pub address: String,
    fd: Arc<OwnedFd>,
}

impl Link {
    pub fn send(&self, data: &[u8]) -> bool {
        // SAFETY: valid fd owned by self.fd, buffer valid for data.len() bytes
        let n = unsafe { libc::write(self.fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
        n == data.len() as isize
    }

    /// Wakes the reader thread, which then reports `Event::Down`.
    pub fn close(&self) {
        // SAFETY: valid fd
        unsafe { libc::shutdown(self.fd.as_raw_fd(), libc::SHUT_RDWR) };
    }
}

pub enum Event {
    Up(Link),
    Packet(String, Vec<u8>),
    Down(String),
}

#[repr(C)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16,
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

fn parse_addr(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = s.split(':');
    // bdaddr_t is little-endian: last octet first
    for slot in out.iter_mut().rev() {
        *slot = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

fn open_socket(addr: [u8; 6]) -> std::io::Result<OwnedFd> {
    // SAFETY: plain socket syscall; the fd is owned right after
    let raw = unsafe { libc::socket(libc::AF_BLUETOOTH, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, BTPROTO_L2CAP) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: raw is a fresh, valid fd
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let sa = SockaddrL2 {
        l2_family: libc::AF_BLUETOOTH as libc::sa_family_t,
        l2_psm: PSM.to_le(),
        l2_bdaddr: addr,
        l2_cid: 0,
        l2_bdaddr_type: 0, // BDADDR_BREDR
    };
    // SAFETY: sa is a properly initialised sockaddr_l2
    let r = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&sa as *const SockaddrL2).cast(),
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

/// Opens the channel on a thread; results arrive as events.
pub fn spawn<E: From<Event> + Send + 'static>(address: String, tx: UnboundedSender<E>) {
    std::thread::spawn(move || {
        let Some(bd) = parse_addr(&address) else {
            let _ = tx.send(Event::Down(address).into());
            return;
        };
        // AirPods accept the channel a moment after the ACL link comes up
        let mut fd = None;
        for attempt in 0..5 {
            std::thread::sleep(Duration::from_millis(if attempt == 0 { 500 } else { 1500 }));
            match open_socket(bd) {
                Ok(f) => {
                    fd = Some(f);
                    break;
                }
                Err(e) => eprintln!("podbar: AAP connect to {address} failed: {e}"),
            }
        }
        let Some(fd) = fd else {
            let _ = tx.send(Event::Down(address).into());
            return;
        };
        let link = Link { address: address.clone(), fd: Arc::new(fd) };
        link.send(HANDSHAKE);
        if tx.send(Event::Up(link.clone()).into()).is_err() {
            return;
        }
        let mut buf = [0u8; 1024];
        loop {
            // SAFETY: buf is valid for buf.len() bytes
            let n = unsafe { libc::read(link.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            let data = buf[..n as usize].to_vec();
            match parse(&data) {
                Some(Msg::HandshakeAck) => {
                    link.send(SET_FEATURES);
                }
                Some(Msg::FeaturesAck) => {
                    link.send(REQUEST_NOTIFICATIONS);
                }
                _ => {}
            }
            if tx.send(Event::Packet(address.clone(), data).into()).is_err() {
                return;
            }
        }
        let _ = tx.send(Event::Down(address).into());
    });
}

/// Some firmwares never ack the feature packet: ask for notifications anyway.
pub fn request_notifications(link: &Link) {
    link.send(REQUEST_NOTIFICATIONS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery() {
        let p = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, 0x02, 0x01, 0x55, 0x02, 0x01, 0x04, 0x01, 0x5a, 0x01, 0x01, 0x08,
            0x01, 0x3c, 0x04, 0x01,
        ];
        assert_eq!(
            parse(&p),
            Some(Msg::Battery(vec![
                (Part::Right, Some(Level { percent: 85, charging: false })),
                (Part::Left, Some(Level { percent: 90, charging: true })),
                (Part::Case, None),
            ]))
        );
    }

    #[test]
    fn noise_roundtrip() {
        for n in Noise::ALL {
            assert_eq!(parse(&noise_packet(n)), Some(Msg::Noise(n)));
        }
    }

    #[test]
    fn ear() {
        assert_eq!(parse(&[4, 0, 4, 0, 6, 0, 0, 2]), Some(Msg::Ear(Place::InEar, Place::InCase)));
    }

    #[test]
    fn addr() {
        assert_eq!(parse_addr("50:F3:51:B0:1C:7D"), Some([0x7d, 0x1c, 0xb0, 0x51, 0xf3, 0x50]));
        assert_eq!(parse_addr("50:F3"), None);
    }

    #[test]
    fn sockaddr_size() {
        assert_eq!(std::mem::size_of::<SockaddrL2>(), 14);
    }
}
