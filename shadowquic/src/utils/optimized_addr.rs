use bytes::{BufMut, Bytes, BytesMut};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompactSocksAddr {
    addr_type: u8,
    port: u16,
    data: Bytes,
}

impl CompactSocksAddr {
    pub fn from_ipv4(ip: [u8; 4], port: u16) -> Self {
        Self {
            addr_type: 1,
            port,
            data: Bytes::copy_from_slice(&ip),
        }
    }

    pub fn from_ipv6(ip: [u8; 16], port: u16) -> Self {
        Self {
            addr_type: 4,
            port,
            data: Bytes::copy_from_slice(&ip),
        }
    }

    pub fn from_domain(domain: &str, port: u16) -> Self {
        let mut buf = BytesMut::with_capacity(1 + domain.len());
        buf.put_u8(domain.len() as u8);
        buf.put_slice(domain.as_bytes());
        Self {
            addr_type: 3,
            port,
            data: buf.freeze(),
        }
    }

    pub fn from_domain_bytes(domain: Bytes, port: u16) -> Self {
        Self {
            addr_type: 3,
            port,
            data: domain,
        }
    }

    #[inline]
    pub fn addr_type(&self) -> u8 {
        self.addr_type
    }

    #[inline]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[inline]
    pub fn as_bytes(&self) -> &Bytes {
        &self.data
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u8(self.addr_type);
        buf.extend_from_slice(&self.data);
        buf.put_u16(self.port);
    }

    pub fn decode(data: &mut Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }
        let addr_type = data[0];
        let data = data.slice(1..);

        match addr_type {
            1 => {
                if data.len() < 6 {
                    return None;
                }
                let ip = data.slice(..4);
                let port = u16::from_be_bytes([data[4], data[5]]);
                Some(Self {
                    addr_type,
                    port,
                    data: ip,
                })
            }
            4 => {
                if data.len() < 18 {
                    return None;
                }
                let ip = data.slice(..16);
                let port = u16::from_be_bytes([data[16], data[17]]);
                Some(Self {
                    addr_type,
                    port,
                    data: ip,
                })
            }
            3 => {
                if data.is_empty() {
                    return None;
                }
                let len = data[0] as usize;
                if data.len() < 1 + len + 2 {
                    return None;
                }
                let domain = data.slice(1..1 + len);
                let port = u16::from_be_bytes([data[1 + len], data[1 + len + 1]]);
                Some(Self {
                    addr_type,
                    port,
                    data: domain,
                })
            }
            _ => None,
        }
    }
}

pub fn fast_hash(addr: &CompactSocksAddr) -> u64 {
    use rustc_hash::FxHasher;
    use std::hash::{Hash, Hasher};
    let mut h = FxHasher::default();
    addr.data.hash(&mut h);
    (addr.port as u64).hash(&mut h);
    h.finish()
}
