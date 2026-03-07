use bytes::Bytes;
use std::{pin::Pin, sync::Arc};

use ahash::AHashMap;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    sync::mpsc::{Sender, channel},
};
use tracing::{Instrument, Level, event, info, trace, trace_span};

use crate::{
    ProxyRequest, TcpSession, TcpTrait, UdpSession,
    error::SError,
    msgs::{
        SDecode,
        socks5::SocksAddr,
        squic::{SQReq, SunnyCredential},
    },
    quic::QuicConnection,
    squic::wait_sunny_auth,
};

use super::{SQConn, handle_udp_packet_recv, handle_udp_recv_ctrl, handle_udp_send};

pub type SunnyQuicUsers = Arc<AHashMap<SunnyCredential, String>>;

#[derive(Clone)]
pub struct SQServerConn<C: QuicConnection> {
    pub inner: SQConn<C>,
    pub users: SunnyQuicUsers,
}
impl<C: QuicConnection> SQServerConn<C> {
    pub async fn handle_connection(self, req_send: Sender<ProxyRequest>) -> Result<(), SError> {
        let conn = &self.inner;
        event!(
            Level::INFO,
            "incoming from {} accepted",
            conn.remote_address()
        );

        // Spawn single UDP packet handler task
        let conn_clone = self.inner.clone();
        tokio::spawn(async move {
            let _ = handle_udp_packet_recv(conn_clone).in_current_span().await;
        });

        while conn.close_reason().is_none() {
            select! {
                bi = conn.accept_bi() => {
                    let (send, recv, id) = bi?;
                    let span = trace_span!("bistream", id = id);
                    trace!("bistream accepted");

                    // Clone only what's needed, not entire self
                    let users = self.users.clone();
                    let req_send = req_send.clone();
                    let inner = self.inner.clone();

                    tokio::spawn(
                        Self::handle_bistream_optimized(users, inner, send, recv, req_send)
                            .instrument(span)
                    );
                },
            }
        }
        Ok(())
    }

    // Optimized bistream handler - avoids self clone
    async fn handle_bistream_optimized(
        users: SunnyQuicUsers,
        inner: SQConn<C>,
        send: C::SendStream,
        mut recv: C::RecvStream,
        req_send: Sender<ProxyRequest>,
    ) -> Result<(), SError> {
        let req = SQReq::decode(&mut recv).await?;

        match req {
            SQReq::SQConnect(dst) => {
                wait_sunny_auth(&inner).await?;
                info!(
                    "connect request: {}->{} accepted",
                    inner.remote_address(),
                    dst
                );
                let tcp: TcpSession = TcpSession {
                    stream: Box::new(Unsplit { s: send, r: recv }),
                    dst,
                };
                req_send
                    .send(ProxyRequest::Tcp(tcp))
                    .await
                    .map_err(|_| SError::OutboundUnavailable)?;
            }
            ref req @ (SQReq::SQAssociatOverDatagram(ref dst)
            | SQReq::SQAssociatOverStream(ref dst)) => {
                wait_sunny_auth(&inner).await?;
                info!("association request to {} accepted", dst);

                // Use larger channel buffer for better throughput
                let (local_send, udp_recv) = channel::<(Bytes, SocksAddr)>(512);
                let (udp_send, local_recv) = channel::<(Bytes, SocksAddr)>(512);
                let udp: UdpSession = UdpSession {
                    send: Arc::new(udp_send),
                    recv: Box::new(udp_recv),
                    stream: None,
                    bind_addr: dst.clone(),
                };
                let local_send = Arc::new(local_send);
                let over_stream = matches!(req, SQReq::SQAssociatOverStream(_));

                if req_send.send(ProxyRequest::Udp(udp)).await.is_err() {
                    return Err(SError::OutboundUnavailable)?;
                }

                let fut1 = handle_udp_send(send, Box::new(local_recv), inner.clone(), over_stream);
                let fut2 = handle_udp_recv_ctrl(recv, local_send, inner);
                tokio::try_join!(fut1, fut2)?;
            }
            SQReq::SQAuthenticate(passwd_hash) => {
                if let Some(name) = users.get(passwd_hash.as_ref()) {
                    tracing::info!("user authenticated:{}", name);
                    inner.authed.set(true).expect("repeated authentication!");
                } else {
                    tracing::error!("authentication failed");
                    inner.close(263, &[]);
                    return Err(SError::SunnyAuthError("Wrong password/username".into()));
                }
            }
            _ => {
                unimplemented!()
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct Unsplit<S, R> {
    pub s: S,
    pub r: R,
}
impl<S: AsyncWrite + Unpin + Sync + Send, R: AsyncRead + Unpin + Sync + Send> TcpTrait
    for Unsplit<S, R>
{
}

impl<S: AsyncWrite + Unpin, R: AsyncRead + Unpin> AsyncRead for Unsplit<S, R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.as_mut().r).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin, R: AsyncRead + Unpin> AsyncWrite for Unsplit<S, R> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.as_mut().s).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.as_mut().s).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.as_mut().s).poll_shutdown(cx)
    }
}
