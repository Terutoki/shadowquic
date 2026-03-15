use bytes::Bytes;
use rand::Rng;
use std::{pin::Pin, sync::Arc};

use rustc_hash::FxHashMap;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    sync::mpsc::{Sender, channel},
};
use tracing::{Instrument, debug, error, info, trace, trace_span};

use crate::{
    ProxyRequest, TcpSession, TcpTrait, UdpSession,
    error::SError,
    msgs::frame::{
        ClientHello, ConnectReq, ERROR_OK, FEATURE_UDP, FastConnectReq, Frame, ServerHello,
        UdpAssociateReq,
    },
    msgs::{SDecode, SEncode, socks5::SocksAddr},
    quic::QuicConnection,
    squic::wait_sunny_auth,
};

use super::{SQConn, handle_udp_recv_ctrl, handle_udp_relay_to_outbound, handle_udp_send};

pub type SunnyQuicUsers = Arc<FxHashMap<[u8; 64], String>>;

#[derive(Clone)]
pub struct SQServerConn<C: QuicConnection> {
    pub inner: SQConn<C>,
    pub users: SunnyQuicUsers,
}
impl<C: QuicConnection> SQServerConn<C> {
    pub async fn handle_connection(self, req_send: Sender<ProxyRequest>) -> Result<(), SError> {
        let conn = &self.inner;
        debug!("incoming from {} accepted", conn.remote_address());

        // Spawn UDP relay handler - handles uni stream -> DirectOut
        let conn_clone = self.inner.clone();
        let req_send_clone = req_send.clone();
        tokio::spawn(async move {
            let _ = handle_udp_relay_to_outbound(conn_clone, req_send_clone)
                .in_current_span()
                .await;
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
        users: Arc<FxHashMap<[u8; 64], String>>,
        inner: SQConn<C>,
        mut send: C::SendStream,
        mut recv: C::RecvStream,
        req_send: Sender<ProxyRequest>,
    ) -> Result<(), SError> {
        let frame = Frame::decode(&mut recv).await?;

        match frame {
            // 0-RTT: FastConnect (auth + connect in one message)
            Frame::FastConnect(req) => {
                if let Some(name) = users.get(&req.auth_token) {
                    tracing::info!("FastConnect user authenticated:{}", name);

                    // 发送FastConnectAck响应
                    use crate::msgs::frame::FastConnectAck;
                    let ack = FastConnectAck {
                        status: 0,
                        bind_addr: req.dst.clone(),
                        connection_id: rand::random(),
                        extensions: vec![],
                    };
                    Frame::FastConnectAck(ack).encode(&mut send).await?;

                    inner.authed.set(true).expect("repeated authentication!");

                    // 直接开始数据转发
                    let tcp: TcpSession = TcpSession {
                        stream: Box::new(Unsplit { s: send, r: recv }),
                        dst: req.dst,
                        session_id: None,
                    };
                    req_send
                        .send(ProxyRequest::Tcp(tcp))
                        .await
                        .map_err(|_| SError::OutboundUnavailable)?;

                    // 注意：每个bistream只能处理一个TCP连接
                } else {
                    tracing::error!("FastConnect authentication failed");
                    inner.close(263, &[]);
                    return Err(SError::SunnyAuthError("Wrong password/username".into()));
                }
            }
            Frame::ClientHello(hello) => {
                // 处理握手请求
                if hello.version != 1 {
                    inner.close(263, &[]);
                    return Err(SError::ProtocolViolation);
                }

                if let Some(name) = users.get(&hello.auth_token) {
                    tracing::info!("user authenticated:{}", name);

                    // 发送ServerHello响应
                    let server_hello = ServerHello {
                        version: 1,
                        selected_features: FEATURE_UDP,
                        connection_id: rand::random(),
                        extensions: vec![],
                    };
                    Frame::ServerHello(server_hello).encode(&mut send).await?;

                    inner.authed.set(true).expect("repeated authentication!");

                    // 握手完成后，继续等待后续请求（支持多路复用）
                    let frame = Frame::decode(&mut recv).await?;
                    match frame {
                        Frame::Connect(req) => {
                            info!(
                                "connect request: {}->{} accepted",
                                inner.remote_address(),
                                req.dst
                            );

                            // 直接开始数据转发，无需发送确认
                            let tcp: TcpSession = TcpSession {
                                stream: Box::new(Unsplit { s: send, r: recv }),
                                dst: req.dst,
                                session_id: None,
                            };
                            req_send
                                .send(ProxyRequest::Tcp(tcp))
                                .await
                                .map_err(|_| SError::OutboundUnavailable)?;

                            // 注意：每个bistream只能处理一个TCP连接
                            // 多路复用需要新建bistream
                        }
                        Frame::Fin => {
                            trace!("client closed the connection");
                        }
                        _ => {
                            tracing::warn!("unexpected frame after handshake: {:?}", frame);
                            inner.close(263, &[]);
                            return Err(SError::ProtocolViolation);
                        }
                    }
                } else {
                    tracing::error!("authentication failed");
                    inner.close(263, &[]);
                    return Err(SError::SunnyAuthError("Wrong password/username".into()));
                }
            }
            Frame::Connect(req) => {
                wait_sunny_auth(&inner).await?;
                info!(
                    "connect request: {}->{} accepted",
                    inner.remote_address(),
                    req.dst
                );

                // 直接开始数据转发，无需发送确认
                let tcp: TcpSession = TcpSession {
                    stream: Box::new(Unsplit { s: send, r: recv }),
                    dst: req.dst,
                    session_id: None,
                };
                req_send
                    .send(ProxyRequest::Tcp(tcp))
                    .await
                    .map_err(|_| SError::OutboundUnavailable)?;
            }
            Frame::UdpAssociate(req) => {
                wait_sunny_auth(&inner).await?;
                let dst = req.dst.unwrap_or_else(SocksAddr::unspecified);
                debug!("ShadowQUIC server: received UDP ASSOCIATE, dst: {}", dst);
                debug!("association request to {} accepted", dst);

                // Clone inner for async tasks
                let inner_for_ctrl = inner.clone();
                let inner_for_send = inner.clone();

                // Use larger channel buffer for better throughput
                let (local_send, udp_recv) = channel::<(Bytes, SocksAddr)>(512);
                let (udp_send, local_recv) = channel::<(Bytes, SocksAddr)>(512);
                debug!("ShadowQUIC server: created UDP channels");

                // Store local_send in lock_free_id_table so handle_udp_relay_to_outbound can find it
                // Sender already implements UdpSend (lib.rs:103)
                let any_udp_send: crate::AnyUdpSend = Arc::new(local_send.clone());
                inner
                    .lock_free_id_table
                    .insert(0u16, any_udp_send, dst.clone());
                debug!("ShadowQUIC server: stored UDP session in lock_free_id_table with id 0");

                let udp: UdpSession = UdpSession {
                    send: Arc::new(udp_send),
                    recv: Box::new(udp_recv),
                    stream: None,
                    bind_addr: dst.clone(),
                    session_id: None,
                };
                let local_send = Arc::new(local_send);
                let over_stream = true;

                // Send UDP session to outbound (DirectOut)
                if req_send.send(ProxyRequest::Udp(udp)).await.is_err() {
                    error!("ShadowQUIC server: failed to send UDP request to outbound");
                    return Err(SError::OutboundUnavailable)?;
                }
                debug!("ShadowQUIC server: UDP request sent to outbound");

                // Spawn task to forward client UDP data from bi stream to DirectOut
                // This reads UDP packets that client sends via the bi stream
                let local_send_for_ctrl = local_send.clone();
                tokio::spawn(async move {
                    trace!("ShadowQUIC server: starting handle_udp_recv_ctrl task...");
                    let result =
                        handle_udp_recv_ctrl(recv, local_send_for_ctrl, inner_for_ctrl).await;
                    trace!(
                        "ShadowQUIC server: handle_udp_recv_ctrl finished: {:?}",
                        result
                    );
                });

                // Spawn task to forward client UDP data from bi stream's send side to DirectOut
                // This reads from local_recv (which receives from DirectOut's response channel)
                // Actually local_recv is for sending TO the client, not receiving from DirectOut
                // Let me reconsider - we need a different approach

                // The data flow is:
                // 1. Client -> bi stream -> server -> DirectOut (handle_udp_recv_ctrl reads from recv)
                // 2. DirectOut -> ??? -> client
                // For path 2, DirectOut writes to udp_session.send (udp_send)
                // We need to read from the channel and send back to client via bi stream

                // For now, let's just handle the forward path (client -> DirectOut)
                // The return path will be handled by handle_udp_relay_to_outbound for uni streams

                // Spawn handle_udp_send - this reads from local_recv and sends via uni stream to client
                // This is for the return path: DirectOut -> server -> uni stream -> client
                tokio::spawn(async move {
                    trace!("ShadowQUIC server: starting handle_udp_send task for return path...");
                    let result =
                        handle_udp_send(send, Box::new(local_recv), inner_for_send, over_stream)
                            .await;
                    trace!("ShadowQUIC server: handle_udp_send finished: {:?}", result);
                });

                // Return immediately - UDP relay is now running in background
                debug!("ShadowQUIC server: UDP relay started in background");
            }
            _ => {
                tracing::warn!("unknown frame type received");
                inner.close(263, &[]);
                return Err(SError::ProtocolViolation);
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
