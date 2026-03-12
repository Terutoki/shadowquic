use std::{
    fmt,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    vec,
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use shadowquic_macros::{SDecode, SEncode};

use super::{SDecode, SEncode, SDecodeSync, SEncodeSync};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[rustfmt::skip]
pub mod consts {
    pub const SOCKS5_VERSION:                          u8 = 0x05;

    pub const SOCKS5_AUTH_METHOD_NONE:                 u8 = 0x00;
    pub const SOCKS5_AUTH_METHOD_GSSAPI:               u8 = 0x01;
    pub const SOCKS5_AUTH_METHOD_PASSWORD:             u8 = 0x02;
    pub const SOCKS5_AUTH_METHOD_NOT_ACCEPTABLE:       u8 = 0xff;

    pub const SOCKS5_CMD_TCP_CONNECT:                  u8 = 0x01;
    pub const SOCKS5_CMD_TCP_BIND:                     u8 = 0x02;
    pub const SOCKS5_CMD_UDP_ASSOCIATE:                u8 = 0x03;

    pub const SOCKS5_ADDR_TYPE_IPV4:                   u8 = 0x01;
    pub const SOCKS5_ADDR_TYPE_DOMAIN_NAME:            u8 = 0x03;
    pub const SOCKS5_ADDR_TYPE_IPV6:                   u8 = 0x04;

    pub const SOCKS5_REPLY_SUCCEEDED:                  u8 = 0x00;
    pub const SOCKS5_REPLY_GENERAL_FAILURE:            u8 = 0x01;
    pub const SOCKS5_REPLY_CONNECTION_NOT_ALLOWED:     u8 = 0x02;
    pub const SOCKS5_REPLY_NETWORK_UNREACHABLE:        u8 = 0x03;
    pub const SOCKS5_REPLY_HOST_UNREACHABLE:           u8 = 0x04;
    pub const SOCKS5_REPLY_CONNECTION_REFUSED:         u8 = 0x05;
    pub const SOCKS5_REPLY_TTL_EXPIRED:                u8 = 0x06;
    pub const SOCKS5_REPLY_COMMAND_NOT_SUPPORTED:      u8 = 0x07;
    pub const SOCKS5_REPLY_ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;
    pub const SOCKS5_RESERVE:                          u8 = 0x00;                 
}

pub use consts::*;

use crate::error::SError;

#[derive(Clone, Debug, SDecode, SEncode)]
pub struct AuthReq {
    pub version: u8,
    pub methods: VarVec,
}

#[derive(Clone, Debug, SDecode, SEncode)]
pub struct PasswordAuthReq {
    pub version: u8,
    pub username: VarVec,
    pub password: VarVec,
}

impl SEncodeSync for PasswordAuthReq {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u8(self.version);
        self.username.encode_sync(buf);
        self.password.encode_sync(buf);
    }
}

impl SDecodeSync for PasswordAuthReq {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let version = buf.get_u8();
        let username = VarVec::decode_sync(buf)?;
        let password = VarVec::decode_sync(buf)?;
        Some(PasswordAuthReq {
            version,
            username,
            password,
        })
    }
}

#[derive(Clone, Debug, SDecode, SEncode)]
pub struct PasswordAuthReply {
    pub version: u8,
    pub status: u8,
}

impl SEncodeSync for PasswordAuthReply {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u8(self.version);
        buf.put_u8(self.status);
    }
}

impl SDecodeSync for PasswordAuthReply {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let version = buf.get_u8();
        let status = buf.get_u8();
        Some(PasswordAuthReply { version, status })
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct VarVec {
    pub len: u8,
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct VarBytes {
    pub len: u8,
    pub contents: Bytes,
}

impl VarBytes {
    pub fn new(contents: Bytes) -> Option<Self> {
        let len = contents.len();
        if len > 255 {
            return None;
        }
        Some(VarBytes {
            len: len as u8,
            contents,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.contents[..self.len as usize]
    }
}

impl From<Vec<u8>> for VarBytes {
    fn from(vec: Vec<u8>) -> Self {
        let len = vec.len() as u8;
        VarBytes {
            len,
            contents: vec.into(),
        }
    }
}

impl From<String> for VarBytes {
    fn from(s: String) -> Self {
        let contents = Bytes::from(s);
        let len = contents.len() as u8;
        VarBytes { len, contents }
    }
}

impl SEncodeSync for VarBytes {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        buf.put_u8(self.len);
        buf.extend_from_slice(self.as_slice());
    }
}

impl SDecodeSync for VarBytes {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let len = buf.get_u8() as usize;
        if buf.remaining() < len {
            return None;
        }
        let contents = buf.copy_to_bytes(len);
        Some(VarBytes { len: len as u8, contents })
    }
}

impl From<Vec<u8>> for VarVec {
    fn from(vec: Vec<u8>) -> Self {
        VarVec {
            len: vec.len() as u8,
            contents: vec,
        }
    }
}

#[async_trait::async_trait]
impl SEncode for VarVec {
    async fn encode<T: AsyncWrite + Unpin + Send>(&self, s: &mut T) -> Result<(), SError> {
        s.write_all(&[self.len]).await?;
        s.write_all(&self.contents[0..self.len as usize]).await?;
        Ok(())
    }
}
#[async_trait::async_trait]
impl SDecode for VarVec {
    async fn decode<T: AsyncRead + Unpin + Send>(s: &mut T) -> Result<Self, SError> {
        let mut buf = [0u8; 1];
        s.read_exact(&mut buf).await?;
        let mut buf2 = vec![0u8; buf[0] as usize];
        s.read_exact(&mut buf2).await?;
        Ok(Self {
            len: buf[0],
            contents: buf2,
        })
    }
}

impl SEncodeSync for VarVec {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        buf.put_u8(self.len);
        buf.extend_from_slice(&self.contents[..self.len as usize]);
    }
}

impl SDecodeSync for VarVec {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let len = buf.get_u8() as usize;
        if buf.remaining() < len {
            return None;
        }
        let mut contents = vec![0u8; len];
        buf.copy_to_slice(&mut contents);
        Some(VarVec { len: len as u8, contents })
    }
}

#[derive(Clone, Debug, SDecode, SEncode)]
pub struct AuthReply {
    pub version: u8,
    pub method: u8,
}

#[derive(Clone, Debug, SDecode, SEncode)]
pub struct CmdReq {
    pub version: u8,
    pub cmd: u8,
    pub rsv: u8,
    pub dst: SocksAddr,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, SDecode, SEncode)]
pub struct SocksAddr {
    pub addr: AddrOrDomain,
    pub port: u16,
}
impl SocksAddr {
    pub fn from_domain(name: String, port: u16) -> Self {
        SocksAddr {
            addr: AddrOrDomain::Domain(VarVec {
                len: name.len() as u8,
                contents: name.into_bytes(),
            }),
            port,
        }
    }
}
impl fmt::Display for SocksAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let AddrOrDomain::V6(_) = self.addr {
            write!(f, "[{}]:{}", self.addr, self.port)
        } else {
            write!(f, "{}:{}", self.addr, self.port)
        }
    }
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, SDecode, SEncode)]
#[repr(u8)]
pub enum AddrOrDomain {
    V4([u8; 4]) = SOCKS5_ADDR_TYPE_IPV4,
    V6([u8; 16]) = SOCKS5_ADDR_TYPE_IPV6,
    Domain(VarVec) = SOCKS5_ADDR_TYPE_DOMAIN_NAME,
}
impl fmt::Display for AddrOrDomain {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AddrOrDomain::V4(x) => write!(f, "{}", IpAddr::from(*x))?,
            AddrOrDomain::V6(x) => write!(f, "{}", IpAddr::from(*x))?,
            AddrOrDomain::Domain(var_vec) => write!(
                f,
                "{}",
                String::from_utf8(var_vec.contents.clone()).map_err(|_| fmt::Error)?
            )?,
        }
        Ok(())
    }
}

impl From<SocketAddr> for SocksAddr {
    fn from(value: SocketAddr) -> Self {
        match value {
            SocketAddr::V4(socket_addr_v4) => SocksAddr {
                addr: AddrOrDomain::V4(socket_addr_v4.ip().octets()),
                port: socket_addr_v4.port(),
            },
            SocketAddr::V6(socket_addr_v6) => SocksAddr {
                addr: AddrOrDomain::V6(socket_addr_v6.ip().octets()),
                port: socket_addr_v6.port(),
            },
        }
    }
}
impl ToSocketAddrs for SocksAddr {
    type Iter = vec::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> std::io::Result<vec::IntoIter<SocketAddr>> {
        match &self.addr {
            AddrOrDomain::Domain(x) => (
                std::str::from_utf8(&x.contents).expect("Domain Name is not UTF8"),
                self.port,
            )
                .to_socket_addrs(),
            AddrOrDomain::V4(x) => {
                Ok(vec![SocketAddr::new(IpAddr::from(*x), self.port)].into_iter())
            }
            AddrOrDomain::V6(x) => {
                Ok(vec![SocketAddr::new(IpAddr::from(*x), self.port)].into_iter())
            }
        }
    }
}

#[derive(Clone, Debug, SEncode, SDecode)]
pub struct CmdReply {
    pub version: u8,
    pub rep: u8,
    pub rsv: u8,
    pub bind_addr: SocksAddr,
}

#[derive(SEncode, SDecode)]
pub struct UdpReqHeader {
    pub rsv: u16,
    pub frag: u8,
    pub dst: SocksAddr,
}

impl SEncodeSync for UdpReqHeader {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u16(self.rsv);
        buf.put_u8(self.frag);
        self.dst.encode_sync(buf);
    }
}

impl SDecodeSync for UdpReqHeader {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let rsv = buf.get_u16();
        let frag = buf.get_u8();
        let dst = SocksAddr::decode_sync(buf)?;
        Some(UdpReqHeader { rsv, frag, dst })
    }
}

#[async_trait::async_trait]
impl SDecode for u8 {
    async fn decode<T: AsyncRead + Unpin + Send>(s: &mut T) -> Result<Self, SError> {
        let mut buf = [0u8];
        s.read_exact(&mut buf).await?;
        Ok(buf[0])
    }
}

#[async_trait::async_trait]
impl SEncode for u8 {
    async fn encode<T: AsyncWrite + Unpin + Send>(&self, s: &mut T) -> Result<(), SError> {
        let buf = [*self];
        s.write_all(&buf).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SDecode for u16 {
    async fn decode<T: AsyncRead + Unpin + Send>(s: &mut T) -> Result<Self, SError> {
        let mut buf = [0u8; 2];
        s.read_exact(&mut buf).await?;
        let val = u16::from_be_bytes(buf);
        Ok(val)
    }
}

#[async_trait::async_trait]
impl SEncode for u16 {
    async fn encode<T: AsyncWrite + Unpin + Send>(&self, s: &mut T) -> Result<(), SError> {
        s.write_u16(*self).await?;
        Ok(())
    }
}

impl SEncodeSync for SocksAddr {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        self.addr.encode_sync(buf);
        buf.put_u16(self.port);
    }
}

impl SDecodeSync for SocksAddr {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let addr = AddrOrDomain::decode_sync(buf)?;
        let port = buf.get_u16();
        Some(SocksAddr { addr, port })
    }
}

impl SEncodeSync for AddrOrDomain {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        match self {
            AddrOrDomain::V4(v) => {
                buf.put_u8(SOCKS5_ADDR_TYPE_IPV4);
                buf.extend_from_slice(v);
            }
            AddrOrDomain::V6(v) => {
                buf.put_u8(SOCKS5_ADDR_TYPE_IPV6);
                buf.extend_from_slice(v);
            }
            AddrOrDomain::Domain(v) => {
                buf.put_u8(SOCKS5_ADDR_TYPE_DOMAIN_NAME);
                v.encode_sync(buf);
            }
        }
    }
}

impl SDecodeSync for AddrOrDomain {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let typ = buf.get_u8();
        match typ {
            SOCKS5_ADDR_TYPE_IPV4 => {
                let mut v = [0u8; 4];
                buf.copy_to_slice(&mut v);
                Some(AddrOrDomain::V4(v))
            }
            SOCKS5_ADDR_TYPE_IPV6 => {
                let mut v = [0u8; 16];
                buf.copy_to_slice(&mut v);
                Some(AddrOrDomain::V6(v))
            }
            SOCKS5_ADDR_TYPE_DOMAIN_NAME => {
                let v = VarVec::decode_sync(buf)?;
                Some(AddrOrDomain::Domain(v))
            }
            _ => None,
        }
    }
}

impl SEncodeSync for AuthReq {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        buf.put_u8(self.version);
        self.methods.encode_sync(buf);
    }
}

impl SDecodeSync for AuthReq {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let version = buf.get_u8();
        let methods = VarVec::decode_sync(buf)?;
        Some(AuthReq { version, methods })
    }
}

impl SEncodeSync for AuthReply {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        buf.put_u8(self.version);
        buf.put_u8(self.method);
    }
}

impl SDecodeSync for AuthReply {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let version = buf.get_u8();
        let method = buf.get_u8();
        Some(AuthReply { version, method })
    }
}

impl SEncodeSync for CmdReq {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        buf.put_u8(self.version);
        buf.put_u8(self.cmd);
        buf.put_u8(self.rsv);
        self.dst.encode_sync(buf);
    }
}

impl SDecodeSync for CmdReq {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let version = buf.get_u8();
        let cmd = buf.get_u8();
        let rsv = buf.get_u8();
        let dst = SocksAddr::decode_sync(buf)?;
        Some(CmdReq { version, cmd, rsv, dst })
    }
}

impl SEncodeSync for CmdReply {
    fn encode_sync(&self, buf: &mut bytes::BytesMut) {
        buf.put_u8(self.version);
        buf.put_u8(self.rep);
        buf.put_u8(self.rsv);
        self.bind_addr.encode_sync(buf);
    }
}

impl SDecodeSync for CmdReply {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let version = buf.get_u8();
        let rep = buf.get_u8();
        let rsv = buf.get_u8();
        let bind_addr = SocksAddr::decode_sync(buf)?;
        Some(CmdReply {
            version,
            rep,
            rsv,
            bind_addr,
        })
    }
}
