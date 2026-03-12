use std::{fmt, sync::Arc};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::SError;

#[cfg(test)]
mod socks5_addr_test;

pub mod socks5;
pub mod squic;

pub const VARINT_MAX_SIZE: usize = 4;
pub const VARINT_MAX_VALUE: u32 = 0x3FFFFFFF;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarInt(u32);

impl VarInt {
    pub fn new(v: u32) -> Option<Self> {
        if v <= VARINT_MAX_VALUE {
            Some(VarInt(v))
        } else {
            None
        }
    }

    pub fn inner(self) -> u32 {
        self.0
    }

    pub fn encode_varint(self) -> Bytes {
        let mut buf = BytesMut::with_capacity(VARINT_MAX_SIZE);
        self.encode_varint_to(&mut buf);
        buf.freeze()
    }

    pub fn encode_varint_to<T: BufMut + ?Sized>(self, buf: &mut T) {
        let val = self.0;
        if val < 64 {
            buf.put_u8(val as u8);
        } else if val < 16384 {
            buf.put_u16((val | 0x4000) as u16);
        } else if val < 1073741824 {
            buf.put_u32(val | 0x80000000);
        } else {
            unreachable!("VarInt value too large");
        }
    }

    pub fn decode_varint<T: Buf + ?Sized>(buf: &mut T) -> Option<Self> {
        let first = buf.get_u8();
        if first < 64 {
            Some(VarInt(first as u32))
        } else if first < 128 {
            let val = buf.get_u16();
            Some(VarInt((val & 0x3FFF) as u32))
        } else if first < 192 {
            if buf.remaining() < 3 {
                return None;
            }
            let b0 = buf.get_u8() as u32;
            let b1 = buf.get_u8() as u32;
            let b2 = buf.get_u8() as u32;
            let val = (b0 << 16) | (b1 << 8) | b2;
            Some(VarInt(val))
        } else {
            None
        }
    }

    pub fn encoded_len(self) -> usize {
        let val = self.0;
        if val < 64 {
            1
        } else if val < 16384 {
            2
        } else {
            4
        }
    }
}

impl From<u8> for VarInt {
    fn from(v: u8) -> Self {
        VarInt(v as u32)
    }
}

impl From<u16> for VarInt {
    fn from(v: u16) -> Self {
        VarInt(v as u32)
    }
}

impl From<u32> for VarInt {
    fn from(v: u32) -> Self {
        VarInt(v)
    }
}

impl From<VarInt> for u32 {
    fn from(v: VarInt) -> Self {
        v.0
    }
}

impl fmt::Debug for VarInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VarInt({})", self.0)
    }
}

pub trait SEncodeSync {
    fn encode_sync(&self, buf: &mut BytesMut);
}

pub trait SDecodeSync: Sized {
    fn decode_sync(buf: &mut Bytes) -> Option<Self>;
}

pub struct LengthPrefixed<T> {
    pub msg: T,
}

impl<T: SEncodeSync> SEncodeSync for LengthPrefixed<T> {
    fn encode_sync(&self, buf: &mut BytesMut) {
        let body_start = buf.len();
        self.msg.encode_sync(buf);
        let body_len = buf.len() - body_start;
        let varint = VarInt::new(body_len as u32).expect("message too large");
        let encoded_len = varint.encoded_len();
        let mut result = BytesMut::with_capacity(encoded_len + body_len);
        varint.encode_varint_to(&mut result);
        result.extend_from_slice(&buf[body_start..]);
        buf.truncate(body_start);
        buf.extend_from_slice(&result);
    }
}

impl<T: SDecodeSync> SDecodeSync for LengthPrefixed<T> {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        let varint = VarInt::decode_varint(buf)?;
        let len = varint.inner() as usize;
        if buf.remaining() < len {
            return None;
        }
        let mut body = buf.copy_to_bytes(len);
        let msg = T::decode_sync(&mut body)?;
        Some(LengthPrefixed { msg })
    }
}

#[derive(Clone)]
pub struct BytesPool {
    chunk_size: usize,
}

impl BytesPool {
    pub fn new(chunk_size: usize) -> Self {
        Self { chunk_size }
    }

    pub fn alloc(&self) -> BytesMut {
        BytesMut::with_capacity(self.chunk_size)
    }

    pub fn alloc_with_capacity(&self, capacity: usize) -> BytesMut {
        BytesMut::with_capacity(capacity)
    }
}

impl Default for BytesPool {
    fn default() -> Self {
        Self::new(4096)
    }
}
/// SEncode is a asyc trait for encoding. It can be automatically derived for struct by the SEncode macro
/// as long as fields are SEncode.
/// For enum, the macro will encode discriminant as u8/u16... defined by `#[repr(*)]` before encoding the content. So the enum can be decoded by first reading a u8/u16...  
/// and then decoding the content based on the value of disriminant.
/// For enum, at most one field is supported.
/// named field is not supported for enum for SDecode macro.
/// `#[repr(*)]` is required for enum to specify the type of discriminant.
#[async_trait::async_trait]
pub trait SEncode {
    async fn encode<T: AsyncWrite + Unpin + Send>(&self, s: &mut T) -> Result<(), SError>;
}

/// A async decoding trait. It can be automatically derived for struct by the SDecode macro as long as fields are SDecode.
/// For enum, the macro will first read a u8/u16... defined by `#[repr(*)]` as discriminant and then decode the content based on the value of disriminant.
/// At most one field is supported for enum. Named field is not supported for enum for SDecode macro.
/// `#[repr(*)]` is required for enum to specify the type of discriminant.
#[async_trait::async_trait]
pub trait SDecode
where
    Self: Sized,
{
    async fn decode<T: AsyncRead + Unpin + Send>(s: &mut T) -> Result<Self, SError>;
}

#[async_trait::async_trait]
impl<S: SEncode + Send + Sync> SEncode for Arc<S> {
    async fn encode<T: AsyncWrite + Unpin + Send>(&self, s: &mut T) -> Result<(), SError> {
        self.as_ref().encode(s).await?;
        Ok(())
    }
}
#[async_trait::async_trait]
impl<S: SDecode> SDecode for Arc<S> {
    async fn decode<T: AsyncRead + Unpin + Send>(s: &mut T) -> Result<Self, SError> {
        let data = S::decode(s).await?;
        Ok(Arc::new(data))
    }
}
#[async_trait::async_trait]
impl<const N: usize> SEncode for [u8; N] {
    async fn encode<T: AsyncWrite + Unpin + Send>(&self, s: &mut T) -> Result<(), SError> {
        s.write_all(self).await?;
        Ok(())
    }
}
#[async_trait::async_trait]
impl<const N: usize> SDecode for [u8; N] {
    async fn decode<T: AsyncRead + Unpin + Send>(s: &mut T) -> Result<Self, SError> {
        let mut data = [0u8; N];
        s.read_exact(&mut data).await?;
        Ok(data)
    }
}

impl SEncodeSync for u8 {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u8(*self);
    }
}

impl SDecodeSync for u8 {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        buf.get_u8().into()
    }
}

impl SEncodeSync for u16 {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u16(*self);
    }
}

impl SDecodeSync for u16 {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        buf.get_u16().into()
    }
}

impl SEncodeSync for u32 {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.put_u32(*self);
    }
}

impl SDecodeSync for u32 {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        buf.get_u32().into()
    }
}

impl SEncodeSync for Bytes {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.extend_from_slice(self);
    }
}

impl SEncodeSync for BytesMut {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.extend_from_slice(self);
    }
}

impl SDecodeSync for Bytes {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        Some(buf.copy_to_bytes(buf.remaining()))
    }
}

impl<const N: usize> SEncodeSync for [u8; N] {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.extend_from_slice(self);
    }
}

impl SEncodeSync for Arc<[u8]> {
    fn encode_sync(&self, buf: &mut BytesMut) {
        buf.extend_from_slice(self.as_ref());
    }
}

impl<S: SEncodeSync + Send + Sync> SEncodeSync for Arc<S> {
    fn encode_sync(&self, buf: &mut BytesMut) {
        self.as_ref().encode_sync(buf);
    }
}

impl<S: SDecodeSync> SDecodeSync for Arc<S> {
    fn decode_sync(buf: &mut Bytes) -> Option<Self> {
        S::decode_sync(buf).map(Arc::new)
    }
}
