use std::{net::SocketAddr, sync::Arc, sync::OnceLock};
use std::net::ToSocketAddrs;

use async_trait::async_trait;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::net::UdpSocket;
use tracing::warn;

use crate::{
    UdpRecv, UdpSend,
    error::SError,
    msgs::socks5::{self, SocksAddr, UdpReqHeader},
    msgs::{SDecodeSync, SEncodeSync},
    utils::memory_pool::fast_alloc,
};

pub mod inbound;
pub mod outbound;

#[derive(Clone)]
pub struct UdpSocksWrap {
    socket: Arc<UdpSocket>,
    remote_addr: OnceLock<SocketAddr>,
    expect_header: bool,
}

impl UdpSocksWrap {
    pub fn new(socket: Arc<UdpSocket>, expect_header: bool) -> Self {
        Self {
            socket,
            remote_addr: Default::default(),
            expect_header,
        }
    }
}

#[async_trait]
impl UdpRecv for UdpSocksWrap {
    async fn recv_from(&mut self) -> Result<(Bytes, SocksAddr), SError> {
        let mut buf = fast_alloc(2000);
        buf.resize(2000, 0);

        let (len, dst) = self.socket.recv_from(&mut buf).await?;

        if self.expect_header {
            let mut bytes = Bytes::copy_from_slice(&buf[..len]);
            let remaining_before = bytes.remaining();
            let req = UdpReqHeader::decode_sync(&mut bytes).ok_or(SError::ProtocolViolation)?;
            if req.frag != 0 {
                warn!("dropping fragmented udp datagram ");
                return Err(SError::ProtocolUnimpl);
            }
            let headsize = remaining_before - bytes.remaining();
            let _ = self.remote_addr.get_or_init(|| dst);
            let buf = buf.freeze();
            Ok((buf.slice(headsize..len), req.dst))
        } else {
            let _ = self.remote_addr.get_or_init(|| dst);
            let buf = buf.freeze();
            Ok((buf.slice(..len), dst.into()))
        }
    }
}
#[async_trait]
impl UdpSend for UdpSocksWrap {
    async fn send_to(&self, buf: Bytes, addr: SocksAddr) -> Result<usize, SError> {
        if self.expect_header {
            let reply = UdpReqHeader {
                rsv: 0,
                frag: 0,
                dst: addr,
            };
            let mut header = BytesMut::with_capacity(64);
            reply.encode_sync(&mut header);

            let total_size = header.len() + buf.len();
            let mut buf_new = fast_alloc(total_size);
            buf_new.put_slice(&header);
            buf_new.put(buf);

            Ok(self.socket.send(&buf_new).await?)
        } else {
            let socket_addr = addr
                .to_socket_addrs()
                .map_err(|_| SError::ProtocolViolation)?
                .next()
                .ok_or(SError::ProtocolViolation)?;
            let buf_ref: &[u8] = &buf;
            Ok(self.socket.send_to(buf_ref, socket_addr).await?)
        }
    }
}
