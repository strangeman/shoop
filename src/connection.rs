extern crate udt;

use std::net::{UdpSocket, SocketAddr, IpAddr};
use std::str;
use std::fmt;
use udt::{UdtSocket, UdtError, UdtOpts, SocketType, SocketFamily};
use sodiumoxide;
use sodiumoxide::crypto::secretbox::xsalsa20poly1305::Key;

// TODO config
const UDT_BUF_SIZE: i32 = 1024000;
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024; // 1 MB max datagram

mod crypto {
    use sodiumoxide::crypto::secretbox;
    use sodiumoxide::crypto::secretbox::xsalsa20poly1305::{NONCEBYTES, Key, Nonce};

    pub fn seal(buf: &[u8], key: &Key) -> Vec<u8> {
        assert!(NONCEBYTES < u8::max_value() as usize,
                "Uh, why is the nonce size this big?");

        let nonce = secretbox::gen_nonce();
        let Nonce(noncebytes) = nonce;
        let mut hdr = vec![0u8; 1 + NONCEBYTES];
        hdr[0] = NONCEBYTES as u8;
        hdr[1..].clone_from_slice(&noncebytes);

        let mut sealed = secretbox::seal(&buf[..], &nonce, key);
        let mut msg = Vec::with_capacity(hdr.len() + sealed.len());
        msg.extend_from_slice(&hdr);
        msg.append(&mut sealed);
        msg
    }

    pub fn open(buf: &[u8], key: &Key) -> Result<Vec<u8>, String> {
        let noncelen = buf[0] as usize;
        if noncelen != NONCEBYTES {
            return Err(String::from("nonce length not recognized"));
        }
        if buf.len() < (1 + noncelen) {
            return Err(String::from("msg not long enough to contain nonce"));
        }
        let mut noncebytes = [0u8; NONCEBYTES];
        noncebytes.copy_from_slice(&buf[1..1 + noncelen]);
        let nonce = Nonce(noncebytes);

        secretbox::open(&buf[1 + noncelen..], &nonce, key)
            .map_err(|_| String::from("failed to decrypt"))
    }

    // Tests for the crypto module
    #[cfg(test)]
    mod test {
        use ::rand;
        use ::rand::distributions::{IndependentSample, Range};

        #[test]
        fn roundtrip() {
            use sodiumoxide::crypto::secretbox;
            // generate some data, seal it, and then make sure it unseals to the same thing
            let mut rng = rand::thread_rng();
            let between = Range::new(10, 10000);


            let key = secretbox::gen_key();
            let data_size: usize = between.ind_sample(&mut rng);
            let mut data = Vec::with_capacity(data_size);
            for _ in 0..data_size {
                data.push(rand::random());
            }

            let cipher_text = super::seal(&data, &key);
            let decrypted_text = super::open(&cipher_text, &key).unwrap();
            assert_eq!(decrypted_text, data);
        }

        #[test]
        fn key_sanity() {
            use std::collections::HashSet;
            use sodiumoxide::crypto::secretbox;
            use sodiumoxide::crypto::secretbox::xsalsa20poly1305::Key;

            let mut set: HashSet<[u8;32]> = HashSet::with_capacity(10000);

            for _ in 0..10000 {
                let key = secretbox::gen_key();
                let Key(keybytes) = key;
                assert!(set.insert(keybytes));
            }
        }

        #[test]
        fn nonce_sanity() {
            use std::collections::HashSet;
            use sodiumoxide::crypto::secretbox;
            use sodiumoxide::crypto::secretbox::xsalsa20poly1305::Nonce;

            let mut set: HashSet<[u8;24]> = HashSet::with_capacity(10000);

            for _ in 0..10000 {
                let nonce = secretbox::gen_nonce();
                let Nonce(noncebytes) = nonce;
                assert!(set.insert(noncebytes));
            }
        }
    }

    #[cfg(all(feature = "nightly", test))]
    mod bench {
        extern crate test;

        #[bench]
        fn bench_seal(b: &mut test::Bencher) {
            use sodiumoxide::crypto::secretbox;
            let key = secretbox::gen_key();
            b.iter(move || super::seal(&[0; 1300], &key))
        }

        #[bench]
        fn bench_gen_key(b: &mut test::Bencher) {
            use sodiumoxide::crypto::secretbox;
            b.iter(|| secretbox::gen_key());
        }
    }
}

fn assert_crypto_init() {
    assert!(sodiumoxide::init(), "Failed to initialize crypto library.");
}

fn new_udt_socket() -> UdtSocket {
    udt::init();
    let sock = UdtSocket::new(SocketFamily::AFInet, SocketType::Datagram).unwrap();
    sock.setsockopt(UdtOpts::UDP_RCVBUF, UDT_BUF_SIZE).unwrap();
    sock.setsockopt(UdtOpts::UDP_SNDBUF, UDT_BUF_SIZE).unwrap();
    sock
}

fn send(sock: &UdtSocket, key: &Key, buf: &[u8]) -> Result<(), UdtError> {
    // FIXME don't unwrap, create an Error struct that can handle everything
    sock.sendmsg(&crypto::seal(buf, key)[..]).map(|_| ())
}

fn recv(sock: &UdtSocket, key: &Key, buf: &mut [u8]) -> Result<Vec<u8>, UdtError> {
    let size = try!(sock.recvmsg(buf));
    crypto::open(&buf[..size], key).map_err(|_| {
        UdtError {
            err_code: -1,
            err_msg: String::from("decryption failure"),
        }
    })
}

pub struct PortRange {
    start: u16,
    end: u16,
}

pub trait Transceiver {
    fn send(&self, buf: &[u8]) -> Result<(), UdtError>;
    fn recv(&self, buf: &mut [u8; MAX_MESSAGE_SIZE]) -> Result<Vec<u8>, UdtError>;
    fn close(&self) -> Result<(), UdtError>;
}

pub struct Server {
    pub ip_addr: IpAddr,
    pub port: u16,
    key: Key,
    sock: UdtSocket,
}

pub struct Client {
    addr: SocketAddr,
    sock: UdtSocket,
    key: Key,
}

pub struct ServerConnection<'a> {
    key: &'a Key,
    sock: UdtSocket,
}

impl Client {
    pub fn new(addr: SocketAddr, key: Key) -> Client {
        assert_crypto_init();
        let sock = new_udt_socket();
        Client {
            addr: addr,
            sock: sock,
            key: key,
        }
    }

    pub fn connect(&self) -> Result<(), UdtError> {
        self.sock.connect(self.addr)
    }
}

impl Transceiver for Client {
    fn send(&self, buf: &[u8]) -> Result<(), UdtError> {
        send(&self.sock, &self.key, buf)
    }

    fn recv(&self, buf: &mut [u8; MAX_MESSAGE_SIZE]) -> Result<Vec<u8>, UdtError> {
        recv(&self.sock, &self.key, buf)
    }

    fn close(&self) -> Result<(), UdtError> {
        self.sock.close()
    }
}

impl Server {
    pub fn get_open_port(range: &PortRange) -> Result<u16, ()> {
        for p in range.start..range.end {
            if let Ok(_) = UdpSocket::bind(&format!("0.0.0.0:{}", p)[..]) {
                return Ok(p);
            }
        }
        Err(())
    }

    pub fn new(ip_addr: IpAddr, port: u16, key: Key) -> Server {
        assert_crypto_init();
        let sock = new_udt_socket();
        sock.bind(SocketAddr::new(ip_addr, port)).unwrap();
        Server {
            sock: sock,
            ip_addr: ip_addr,
            port: port,
            key: key,
        }
    }

    pub fn listen(&self) -> Result<(), UdtError> {
        self.sock.listen(2)
    }

    pub fn accept(&self) -> Result<ServerConnection, UdtError> {
        self.sock.accept().map(|(sock, _)| {
            ServerConnection {
                key: &self.key,
                sock: sock,
            }
        })
    }
}

impl<'a> ServerConnection<'a> {
    pub fn getpeer(&self) -> Result<SocketAddr, UdtError> {
        self.sock.getpeername()
    }
}

impl<'a> Transceiver for ServerConnection<'a> {
    fn send(&self, buf: &[u8]) -> Result<(), UdtError> {
        send(&self.sock, self.key, buf)
    }

    fn recv(&self, buf: &mut [u8; MAX_MESSAGE_SIZE]) -> Result<Vec<u8>, UdtError> {
        recv(&self.sock, self.key, buf)
    }

    fn close(&self) -> Result<(), UdtError> {
        self.sock.close()
    }
}

impl<'a> PortRange {
    fn new(start: u16, end: u16) -> Result<PortRange, &'a str> {
        if start > end {
            Err("range end must be greater than or equal to start")
        } else {
            Ok(PortRange {
                start: start,
                end: end,
            })
        }
    }

    pub fn from(s: &str) -> Result<PortRange, &'a str> {
        let sections: Vec<&str> = s.split('-').collect();
        if sections.len() != 2 {
            return Err("Range must be specified in the form of \"<start>-<end>\"");
        }
        let (start, end) = (sections[0].parse::<u16>(), sections[1].parse::<u16>());
        if start.is_err() || end.is_err() {
            return Err("improperly formatted port range");
        }
        PortRange::new(start.unwrap(), end.unwrap())
    }
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}
