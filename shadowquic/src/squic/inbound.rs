use bytes::Bytes;
use rand::Rng;
use std::{pin::Pin, sync::Arc};

use rustc_hash::FxHashMap;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    sync::mpsc::{Sender, channel},
};
use tracing::{Instrument, Level, event, info, trace, trace_span};

use crate::{
    ProxyRequest, TcpSession, TcpTrait, UdpSession,
    error::SError,
    msgs::frame::{ClientHello, ConnectReq, FastConnectReq, Frame, ServerHello, UdpAssociateReq, ERROR_OK, FEATURE_UDP},
    msgs::{
        SDecode, SEncode,
        socks5::SocksAddr,
    },
    quic::QuicConnection,
    squic::wait_sunny_auth,
};

use super::{SQConn, handle_udp_packet_recv, handle_udp_recv_ctrl, handle_udp_send};

pub type SunnyQuicUsers = Arc<FxHashMap<[u8; 64], String>>;

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
                let dst = req.dst.ok_or(SError::ProtocolViolation)?;
                info!("association request to {} accepted", dst);

                // Use larger channel buffer for better throughput
                let (local_send, udp_recv) = channel::<(Bytes, SocksAddr)>(512);
                let (udp_send, local_recv) = channel::<(Bytes, SocksAddr)>(512);
                let udp: UdpSession = UdpSession {
                    send: Arc::new(udp_send),
                    recv: Box::new(udp_recv),
                    stream: None,
                    bind_addr: dst.clone(),
                    session_id: None,
                };
                let local_send = Arc::new(local_send);
                let over_stream = true; // 新协议统一使用流模式

                if req_send.send(ProxyRequest::Udp(udp)).await.is_err() {
                    return Err(SError::OutboundUnavailable)?;
                }

                let fut1 = handle_udp_send(send, Box::new(local_recv), inner.clone(), over_stream);
                let fut2 = handle_udp_recv_ctrl(recv, local_send, inner);
                tokio::try_join!(fut1, fut2)?;
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
