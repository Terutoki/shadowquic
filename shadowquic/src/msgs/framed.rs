use std::io::Cursor;

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::SError;

use super::frame::{Frame, FrameHeader};
use super::{SDecode, SDecodeSync, SEncode, SEncodeSync};

/// 帧读写器
pub struct Framed<T> {
    io: T,
    read_buf: BytesMut,
    write_buf: BytesMut,
}

impl<T: AsyncRead + Unpin> Framed<T> {
    pub fn new(io: T) -> Self {
        Self {
            io,
            read_buf: BytesMut::with_capacity(8192),
            write_buf: BytesMut::with_capacity(8192),
        }
    }

    /// 读取一个帧
    pub async fn read_frame(&mut self) -> Result<Frame, SError> {
        loop {
            // 尝试解析帧头
            let mut header_buf = self.read_buf.clone().freeze();
            if let Some(header) = FrameHeader::decode_sync(&mut header_buf) {
                let total_len = header.encoded_len() + header.length.inner() as usize;
                if self.read_buf.len() >= total_len {
                    // 有足够的数据
                    let mut frame_data = self.read_buf.split_to(total_len).freeze();
                    return Frame::decode_sync(&mut frame_data).ok_or(SError::ProtocolViolation);
                }
            }

            // 需要更多数据
            let n = self.io.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return Err(SError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                )));
            }
        }
    }

    /// 写入一个帧
    pub async fn write_frame<W: AsyncWrite + Unpin>(
        &mut self,
        frame: &Frame,
        w: &mut W,
    ) -> Result<(), SError> {
        self.write_buf.clear();
        frame.encode_sync(&mut self.write_buf);
        w.write_all(&self.write_buf).await?;
        Ok(())
    }

    /// 写入一个帧（同步版本，使用内部缓冲区）
    pub async fn write_frame_sync<W: AsyncWrite + Unpin>(
        &mut self,
        frame: &Frame,
        w: &mut W,
    ) -> Result<(), SError> {
        let mut buf = BytesMut::new();
        frame.encode_sync(&mut buf);
        w.write_all(&buf).await?;
        Ok(())
    }

    /// 拆分解包，返回底层IO
    pub fn into_inner(self) -> T {
        self.io
    }

    /// 获取底层IO的引用
    pub fn get_ref(&self) -> &T {
        &self.io
    }

    /// 获取底层IO的可变引用
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.io
    }
}

#[async_trait::async_trait]
impl SEncode for Frame {
    async fn encode<W: AsyncWrite + Unpin + Send>(&self, w: &mut W) -> Result<(), SError> {
        let mut buf = BytesMut::new();
        self.encode_sync(&mut buf);
        w.write_all(&buf).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SDecode for Frame {
    async fn decode<R: AsyncRead + Unpin + Send>(r: &mut R) -> Result<Self, SError> {
        let mut framed = Framed::new(r);
        framed.read_frame().await
    }
}
