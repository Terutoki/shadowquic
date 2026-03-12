use std::sync::Arc;

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::error::SError;

use super::socks5::SocksAddr;
use super::{SDecode, SEncode, SDecodeSync, SEncodeSync};
use shadowquic_macros::{SDecode, SEncode};

pub static SUNNY_QUIC_AUTH_LEN: usize = 64;
pub(crate) type SunnyCredential = Arc<[u8; SUNNY_QUIC_AUTH_LEN]>;

#[derive(PartialEq)]
#[repr(u8)]
#[derive(SEncode, SDecode)]
pub enum SQReq {
    SQConnect(SocksAddr) = 0x1,
    SQBind(SocksAddr) = 0x2,
    SQAssociatOverDatagram(SocksAddr) = 0x3,
    SQAssociatOverStream(SocksAddr) = 0x4,
    SQAuthenticate(SunnyCredential) = 0x5,
}

#[derive(SEncode, SDecode)]
pub struct SQUdpControlHeader {
    pub dst: SocksAddr,
    pub id: u16, // id is one to one coresponance a udpsocket and proxy dst
}

#[derive(SEncode, SDecode)]
pub struct SQPacketStreamHeader {
    pub id: u16, // id is one to one coresponance a udpsocket and proxy dst
    pub len: u16,
}

#[derive(SEncode, SDecode, Clone)]
pub struct SQPacketDatagramHeader {
    pub id: u16, // id is one to one coresponance a udpsocket and proxy dst
}

impl SEncodeSync for SQReq {
    fn encode_sync(&self, buf: &mut BytesMut) {
        match self {
            SQReq::SQConnect(addr) => {
                buf.put_u8(0x1);
                addr.encode_sync(buf);
            }
            SQReq::SQBind(addr) => {
                buf.put_u8(0x2);
                addr.encode_sync(buf);
            }
            SQReq::SQAssociatOverDatagram(addr) => {
                buf.put_u8(0x3);
                addr.encode_sync(buf);
            }
            SQReq::SQAssociatOverStream(addr) => {
                buf.put_u8(0x4);
                addr.encode_sync(buf);
            }
            SQReq::SQAuthenticate(cred) => {
                buf.put_u8(0x5);
                buf.extend_from_slice(cred.as_ref());
            }
        }
    }
}

impl SDecodeSync for SQReq {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let tag = buf.get_u8();
        match tag {
            0x1 => {
                let addr = SocksAddr::decode_sync(buf)?;
                Some(SQReq::SQConnect(addr))
            }
            0x2 => {
                let addr = SocksAddr::decode_sync(buf)?;
                Some(SQReq::SQBind(addr))
            }
            0x3 => {
                let addr = SocksAddr::decode_sync(buf)?;
                Some(SQReq::SQAssociatOverDatagram(addr))
            }
            0x4 => {
                let addr = SocksAddr::decode_sync(buf)?;
                Some(SQReq::SQAssociatOverStream(addr))
            }
            0x5 => {
                if buf.remaining() < SUNNY_QUIC_AUTH_LEN {
                    return None;
                }
                let mut cred = [0u8; SUNNY_QUIC_AUTH_LEN];
                buf.copy_to_slice(&mut cred);
                Some(SQReq::SQAuthenticate(Arc::new(cred)))
            }
            _ => None,
        }
    }
}

impl SEncodeSync for SQUdpControlHeader {
    fn encode_sync(&self, buf: &mut BytesMut) {
        self.dst.encode_sync(buf);
        buf.put_u16(self.id);
    }
}

impl SDecodeSync for SQUdpControlHeader {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let dst = SocksAddr::decode_sync(buf)?;
        let id = buf.get_u16();
        Some(SQUdpControlHeader { dst, id })
    }
}

impl SEncodeSync for SQPacketStreamHeader {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u16(self.id);
        buf.put_u16(self.len);
    }
}

impl SDecodeSync for SQPacketStreamHeader {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let id = buf.get_u16();
        let len = buf.get_u16();
        Some(SQPacketStreamHeader { id, len })
    }
}

impl SEncodeSync for SQPacketDatagramHeader {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u16(self.id);
    }
}

impl SDecodeSync for SQPacketDatagramHeader {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let id = buf.get_u16();
        Some(SQPacketDatagramHeader { id })
    }
}

#[tokio::test]
async fn test_encode_req() {
    let req = SQReq::SQAuthenticate(Arc::new([1u8; SUNNY_QUIC_AUTH_LEN]));
    let buf = vec![0u8; 1 + SUNNY_QUIC_AUTH_LEN];
    let mut cursor = std::io::Cursor::new(buf);
    req.encode(&mut cursor).await.unwrap();
    assert_eq!(cursor.into_inner()[0], 0x5);
}

#[tokio::test]
async fn test_macro_expand_req() {
    const TEST_CONST: u8 = 89;
    #[repr(u8)]
    #[derive(SDecode, SEncode, PartialEq)]
    #[allow(dead_code)]
    pub enum Cmd {
        Connect,
        Bind = 0x8,
        AssociatOverDatagram,
        AssociatOverStream = TEST_CONST,
        Authenticate,
    }
}
