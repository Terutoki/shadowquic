use std::sync::Arc;

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::error::SError;

use super::{socks5::SocksAddr, SDecode, SDecodeSync, SEncode, SEncodeSync};
use shadowquic_macros::{SDecode, SEncode};

/// 标准QUIC VarInt实现，最大支持2^62-1
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct VarInt(u64);

impl VarInt {
    pub const MAX: u64 = (1 << 62) - 1;

    #[inline]
    pub fn new(v: u64) -> Option<Self> {
        if v <= Self::MAX {
            Some(VarInt(v))
        } else {
            None
        }
    }

    #[inline]
    pub fn inner(self) -> u64 {
        self.0
    }

    #[inline]
    pub fn encode_to(self, buf: &mut BytesMut) -> usize {
        let val = self.0;
        if val < 64 {
            buf.put_u8(val as u8);
            1
        } else if val < 16384 {
            buf.put_u16(((val as u16) & 0x3FFF) | 0x4000);
            2
        } else if val < 1073741824 {
            buf.put_u32(((val as u32) & 0x3FFFFFFF) | 0x80000000);
            4
        } else {
            buf.put_u64((val & 0x3FFFFFFFFFFFFFFF) | 0xC000000000000000);
            8
        }
    }

    #[inline]
    pub fn decode(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.is_empty() {
            return None;
        }

        let first = buf[0];
        let len = 1 << ((first >> 6) & 0x03);

        if buf.len() < len {
            return None;
        }

        let val = match len {
            1 => (first & 0x3F) as u64,
            2 => ((first & 0x3F) as u64) << 8 | buf[1] as u64,
            4 => {
                ((first & 0x3F) as u64) << 24
                    | (buf[1] as u64) << 16
                    | (buf[2] as u64) << 8
                    | buf[3] as u64
            }
            8 => {
                ((first & 0x3F) as u64) << 56
                    | (buf[1] as u64) << 48
                    | (buf[2] as u64) << 40
                    | (buf[3] as u64) << 32
                    | (buf[4] as u64) << 24
                    | (buf[5] as u64) << 16
                    | (buf[6] as u64) << 8
                    | buf[7] as u64
            }
            _ => unreachable!(),
        };

        Some((VarInt(val), len))
    }

    #[inline]
    pub fn encoded_len(self) -> usize {
        let val = self.0;
        if val < 64 {
            1
        } else if val < 16384 {
            2
        } else if val < 1073741824 {
            4
        } else {
            8
        }
    }
}

/// 帧类型
#[repr(u64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameType {
    // 控制帧
    ClientHello = 0x00,
    ServerHello = 0x01,
    Ping = 0x02,
    Pong = 0x03,
    Error = 0x04,

    // 数据帧
    Connect = 0x10,
    ConnectAck = 0x11,
    Data = 0x12,
    Fin = 0x13,
    Reset = 0x14,

    // UDP相关
    UdpAssociate = 0x20,
    UdpData = 0x21,
}

/// 帧标志位
pub mod flags {
    pub const NONE: u8 = 0x00;
    pub const NEED_ACK: u8 = 0x01;
    pub const IS_ACK: u8 = 0x02;
    pub const HAS_EXTENSIONS: u8 = 0x04;
    pub const FIN: u8 = 0x08;
}

/// 统一帧头
#[derive(Debug, Clone)]
pub struct FrameHeader {
    pub version: u8, // 协议版本 (当前为1)
    pub frame_type: FrameType,
    pub flags: u8,
    pub stream_id: u32, // 流ID，支持42亿并发流
    pub length: VarInt, // 负载长度
}

/// 扩展字段
#[derive(Debug, Clone)]
pub struct Extension {
    pub typ: VarInt,
    pub value: Bytes,
}

/// 客户端握手消息
#[derive(Debug, Clone)]
pub struct ClientHello {
    pub version: u8,
    pub supported_features: u64, // 特性位掩码
    pub auth_token: [u8; 64],    // 64字节认证token
    pub extensions: Vec<Extension>,
}

/// 服务端握手消息
#[derive(Debug, Clone)]
pub struct ServerHello {
    pub version: u8,
    pub selected_features: u64,
    pub connection_id: u64,
    pub extensions: Vec<Extension>,
}

/// 连接请求
#[derive(Debug, Clone)]
pub struct ConnectReq {
    pub dst: SocksAddr,
    pub extensions: Vec<Extension>,
}

/// 连接响应
#[derive(Debug, Clone)]
pub struct ConnectAck {
    pub status: u16,
    pub bind_addr: SocksAddr,
    pub extensions: Vec<Extension>,
}

/// UDP关联请求
#[derive(Debug, Clone)]
pub struct UdpAssociateReq {
    pub dst: Option<SocksAddr>, // 固定目标地址，None表示任意地址
    pub extensions: Vec<Extension>,
}

/// UDP数据帧
#[derive(Debug, Clone)]
pub struct UdpData {
    pub dst: Option<SocksAddr>, // 目标地址，固定目标时省略
    pub payload: Bytes,
}

/// 错误帧
#[derive(Debug, Clone)]
pub struct ErrorFrame {
    pub code: u32,
    pub message: Bytes,
}

/// 统一帧结构
#[derive(Debug, Clone)]
pub enum Frame {
    ClientHello(ClientHello),
    ServerHello(ServerHello),
    Ping(u64),
    Pong(u64),
    Error(ErrorFrame),
    Connect(ConnectReq),
    ConnectAck(ConnectAck),
    Data(Bytes),
    Fin,
    Reset(u32),
    UdpAssociate(UdpAssociateReq),
    UdpData(UdpData),
}

impl FrameHeader {
    #[inline]
    pub fn encoded_len(&self) -> usize {
        1 + // version_type
        8 + // type_low
        1 + // flags
        4 + // stream_id
        self.length.encoded_len()
    }
}

impl SEncodeSync for FrameHeader {
    fn encode_sync(&self, buf: &mut BytesMut) {
        // 版本(2位) + 帧类型高6位
        let version_type = (self.version << 6) | ((self.frame_type as u64 >> 56) as u8 & 0x3F);
        buf.put_u8(version_type);

        // 帧类型低56位
        buf.put_u64(self.frame_type as u64 & 0x00FFFFFFFFFFFFFF);

        buf.put_u8(self.flags);
        buf.put_u32(self.stream_id);
        self.length.encode_to(buf);
    }
}

impl SDecodeSync for FrameHeader {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        // 最小帧头长度: 1(version_type) + 8(type) + 1(flags) + 4(stream_id) + 1(min varint) = 15
        if buf.remaining() < 15 {
            return None;
        }

        let version_type = buf.get_u8();
        let version = (version_type >> 6) & 0x03;
        let type_high = (version_type & 0x3F) as u64;
        let type_low = buf.get_u64();
        let frame_type_val = (type_high << 56) | type_low;

        let frame_type = match frame_type_val {
            0x00 => FrameType::ClientHello,
            0x01 => FrameType::ServerHello,
            0x02 => FrameType::Ping,
            0x03 => FrameType::Pong,
            0x04 => FrameType::Error,
            0x10 => FrameType::Connect,
            0x11 => FrameType::ConnectAck,
            0x12 => FrameType::Data,
            0x13 => FrameType::Fin,
            0x14 => FrameType::Reset,
            0x20 => FrameType::UdpAssociate,
            0x21 => FrameType::UdpData,
            _ => return None,
        };

        let flags = buf.get_u8();
        let stream_id = buf.get_u32();

        let (length, len_size) = VarInt::decode(buf.chunk())?;
        buf.advance(len_size);

        Some(FrameHeader {
            version,
            frame_type,
            flags,
            stream_id,
            length,
        })
    }
}

// 错误码定义
pub const ERROR_OK: u32 = 0x0000;
pub const ERROR_AUTH_FAILED: u32 = 0x0001;
pub const ERROR_NETWORK_UNREACHABLE: u32 = 0x0002;
pub const ERROR_HOST_UNREACHABLE: u32 = 0x0003;
pub const ERROR_CONNECTION_REFUSED: u32 = 0x0004;
pub const ERROR_TTL_EXPIRED: u32 = 0x0005;
pub const ERROR_COMMAND_NOT_SUPPORTED: u32 = 0x0006;
pub const ERROR_ADDR_NOT_SUPPORTED: u32 = 0x0007;
pub const ERROR_PROTOCOL_VIOLATION: u32 = 0x0008;
pub const ERROR_INTERNAL: u32 = 0x0009;

// 特性位定义
pub const FEATURE_UDP: u64 = 1 << 0;
pub const FEATURE_TCP_FAST_OPEN: u64 = 1 << 1;
pub const FEATURE_MULTIPATH: u64 = 1 << 2;
pub const FEATURE_ZSTD_COMPRESSION: u64 = 1 << 3;
pub const FEATURE_METRICS: u64 = 1 << 4;

// 手动实现所有消息类型的编解码
impl SEncodeSync for ClientHello {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u8(self.version);
        buf.put_u64(self.supported_features);
        buf.extend_from_slice(&self.auth_token);
        self.extensions.encode_sync(buf);
    }
}

impl SDecodeSync for ClientHello {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < 1 + 8 + 64 {
            return None;
        }

        let version = buf.get_u8();
        let supported_features = buf.get_u64();
        let mut auth_token = [0u8; 64];
        buf.copy_to_slice(&mut auth_token);
        let extensions = Vec::<Extension>::decode_sync(buf)?;

        Some(ClientHello {
            version,
            supported_features,
            auth_token,
            extensions,
        })
    }
}

impl SEncodeSync for ServerHello {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u8(self.version);
        buf.put_u64(self.selected_features);
        buf.put_u64(self.connection_id);
        self.extensions.encode_sync(buf);
    }
}

impl SDecodeSync for ServerHello {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < 1 + 8 + 8 {
            return None;
        }

        let version = buf.get_u8();
        let selected_features = buf.get_u64();
        let connection_id = buf.get_u64();
        let extensions = Vec::<Extension>::decode_sync(buf)?;

        Some(ServerHello {
            version,
            selected_features,
            connection_id,
            extensions,
        })
    }
}

impl SEncodeSync for ConnectReq {
    fn encode_sync(&self, buf: &mut BytesMut) {
        self.dst.encode_sync(buf);
        self.extensions.encode_sync(buf);
    }
}

impl SDecodeSync for ConnectReq {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let dst = SocksAddr::decode_sync(buf)?;
        let extensions = Vec::<Extension>::decode_sync(buf)?;

        Some(ConnectReq { dst, extensions })
    }
}

impl SEncodeSync for ConnectAck {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u16(self.status);
        self.bind_addr.encode_sync(buf);
        self.extensions.encode_sync(buf);
    }
}

impl SDecodeSync for ConnectAck {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < 2 {
            return None;
        }

        let status = buf.get_u16();
        let bind_addr = SocksAddr::decode_sync(buf)?;
        let extensions = Vec::<Extension>::decode_sync(buf)?;

        Some(ConnectAck {
            status,
            bind_addr,
            extensions,
        })
    }
}

impl SEncodeSync for UdpAssociateReq {
    fn encode_sync(&self, buf: &mut BytesMut) {
        match &self.dst {
            Some(dst) => {
                buf.put_u8(1);
                dst.encode_sync(buf);
            }
            None => {
                buf.put_u8(0);
            }
        }
        self.extensions.encode_sync(buf);
    }
}

impl SDecodeSync for UdpAssociateReq {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < 1 {
            return None;
        }

        let has_dst = buf.get_u8();
        let dst = if has_dst == 1 {
            Some(SocksAddr::decode_sync(buf)?)
        } else {
            None
        };

        let extensions = Vec::<Extension>::decode_sync(buf)?;

        Some(UdpAssociateReq { dst, extensions })
    }
}

impl SEncodeSync for UdpData {
    fn encode_sync(&self, buf: &mut BytesMut) {
        match &self.dst {
            Some(dst) => {
                buf.put_u8(1);
                dst.encode_sync(buf);
            }
            None => {
                buf.put_u8(0);
            }
        }
        let payload_len = VarInt::new(self.payload.len() as u64).unwrap();
        payload_len.encode_to(buf);
        buf.extend_from_slice(&self.payload);
    }
}

impl SDecodeSync for UdpData {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < 1 {
            return None;
        }

        let has_dst = buf.get_u8();
        let dst = if has_dst == 1 {
            Some(SocksAddr::decode_sync(buf)?)
        } else {
            None
        };

        let (payload_len, len_size) = VarInt::decode(buf.chunk())?;
        buf.advance(len_size);
        let payload_len = payload_len.inner() as usize;

        if buf.remaining() < payload_len {
            return None;
        }

        let payload = buf.copy_to_bytes(payload_len);

        Some(UdpData { dst, payload })
    }
}

impl SEncodeSync for ErrorFrame {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u32(self.code);
        let msg_len = VarInt::new(self.message.len() as u64).unwrap();
        msg_len.encode_to(buf);
        buf.extend_from_slice(&self.message);
    }
}

impl SDecodeSync for ErrorFrame {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < 4 {
            return None;
        }

        let code = buf.get_u32();
        let (msg_len, len_size) = VarInt::decode(buf.chunk())?;
        buf.advance(len_size);
        let msg_len = msg_len.inner() as usize;

        if buf.remaining() < msg_len {
            return None;
        }

        let message = buf.copy_to_bytes(msg_len);

        Some(ErrorFrame { code, message })
    }
}

impl SEncodeSync for Frame {
    fn encode_sync(&self, buf: &mut BytesMut) {
        let header_start = buf.len();

        // 先预留帧头空间(最大1+8+1+4+8=22字节)
        buf.reserve(22);
        unsafe { buf.set_len(header_start + 22) };

        let payload_start = buf.len();

        // 编码负载
        match self {
            Frame::ClientHello(hello) => hello.encode_sync(buf),
            Frame::ServerHello(hello) => hello.encode_sync(buf),
            Frame::Ping(timestamp) => buf.put_u64(*timestamp),
            Frame::Pong(timestamp) => buf.put_u64(*timestamp),
            Frame::Error(err) => err.encode_sync(buf),
            Frame::Connect(req) => req.encode_sync(buf),
            Frame::ConnectAck(ack) => ack.encode_sync(buf),
            Frame::Data(data) => buf.extend_from_slice(data),
            Frame::Fin => {}
            Frame::Reset(code) => buf.put_u32(*code),
            Frame::UdpAssociate(req) => req.encode_sync(buf),
            Frame::UdpData(data) => data.encode_sync(buf),
        }

        let payload_len = buf.len() - payload_start;
        let length = VarInt::new(payload_len as u64).unwrap();

        // 构建帧头
        let frame_type = match self {
            Frame::ClientHello(_) => FrameType::ClientHello,
            Frame::ServerHello(_) => FrameType::ServerHello,
            Frame::Ping(_) => FrameType::Ping,
            Frame::Pong(_) => FrameType::Pong,
            Frame::Error(_) => FrameType::Error,
            Frame::Connect(_) => FrameType::Connect,
            Frame::ConnectAck(_) => FrameType::ConnectAck,
            Frame::Data(_) => FrameType::Data,
            Frame::Fin => FrameType::Fin,
            Frame::Reset(_) => FrameType::Reset,
            Frame::UdpAssociate(_) => FrameType::UdpAssociate,
            Frame::UdpData(_) => FrameType::UdpData,
        };

        let header = FrameHeader {
            version: 1,
            frame_type,
            flags: match self {
                Frame::Fin => flags::FIN,
                _ => flags::NONE,
            },
            stream_id: 0, // 调用者会设置
            length,
        };

        // 编码帧头到预留位置
        let mut header_buf = BytesMut::with_capacity(22);

        // 版本和类型高6位
        let version_type = (header.version << 6) | ((header.frame_type as u64 >> 56) as u8 & 0x3F);
        header_buf.put_u8(version_type);

        // 类型低56位
        let type_low = header.frame_type as u64 & 0x00FFFFFFFFFFFFFF;
        header_buf.put_u64(type_low);

        header_buf.put_u8(header.flags);
        header_buf.put_u32(header.stream_id);

        let len_size = header.length.encode_to(&mut header_buf);

        // 复制到输出缓冲区
        buf[header_start..header_start + header_buf.len()].copy_from_slice(&header_buf);

        // 移动负载到正确位置
        let total_header_len = 14 + len_size;
        if total_header_len < 22 {
            let shift = 22 - total_header_len;
            buf.copy_within(payload_start.., header_start + total_header_len);
            buf.truncate(buf.len() - shift);
        }
    }
}

impl SDecodeSync for Frame {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let header = FrameHeader::decode_sync(buf)?;
        let payload_len = header.length.inner() as usize;

        if buf.remaining() < payload_len {
            return None;
        }

        let mut payload = buf.split_to(payload_len);

        let frame = match header.frame_type {
            FrameType::ClientHello => {
                let hello = ClientHello::decode_sync(&mut payload)?;
                Frame::ClientHello(hello)
            }
            FrameType::ServerHello => {
                let hello = ServerHello::decode_sync(&mut payload)?;
                Frame::ServerHello(hello)
            }
            FrameType::Ping => {
                let ts = payload.get_u64();
                Frame::Ping(ts)
            }
            FrameType::Pong => {
                let ts = payload.get_u64();
                Frame::Pong(ts)
            }
            FrameType::Error => {
                let err = ErrorFrame::decode_sync(&mut payload)?;
                Frame::Error(err)
            }
            FrameType::Connect => {
                let req = ConnectReq::decode_sync(&mut payload)?;
                Frame::Connect(req)
            }
            FrameType::ConnectAck => {
                let ack = ConnectAck::decode_sync(&mut payload)?;
                Frame::ConnectAck(ack)
            }
            FrameType::Data => Frame::Data(payload),
            FrameType::Fin => Frame::Fin,
            FrameType::Reset => {
                let code = payload.get_u32();
                Frame::Reset(code)
            }
            FrameType::UdpAssociate => {
                let req = UdpAssociateReq::decode_sync(&mut payload)?;
                Frame::UdpAssociate(req)
            }
            FrameType::UdpData => {
                let data = UdpData::decode_sync(&mut payload)?;
                Frame::UdpData(data)
            }
        };

        Some(frame)
    }
}

impl SEncodeSync for Extension {
    fn encode_sync(&self, buf: &mut BytesMut) {
        self.typ.encode_to(buf);
        let value_len = VarInt::new(self.value.len() as u64).unwrap();
        value_len.encode_to(buf);
        buf.extend_from_slice(&self.value);
    }
}

impl SDecodeSync for Extension {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let (typ, typ_size) = VarInt::decode(buf.chunk())?;
        buf.advance(typ_size);

        let (value_len, len_size) = VarInt::decode(buf.chunk())?;
        buf.advance(len_size);
        let value_len = value_len.inner() as usize;

        if buf.remaining() < value_len {
            return None;
        }

        let value = buf.copy_to_bytes(value_len);
        Some(Extension { typ, value })
    }
}

impl SEncodeSync for Vec<Extension> {
    fn encode_sync(&self, buf: &mut BytesMut) {
        let len = VarInt::new(self.len() as u64).unwrap();
        len.encode_to(buf);

        for ext in self {
            ext.encode_sync(buf);
        }
    }
}

impl SDecodeSync for Vec<Extension> {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let (len, len_size) = VarInt::decode(buf.chunk())?;
        buf.advance(len_size);
        let len = len.inner() as usize;

        let mut exts = Vec::with_capacity(len);
        for _ in 0..len {
            let ext = Extension::decode_sync(buf)?;
            exts.push(ext);
        }

        Some(exts)
    }
}
