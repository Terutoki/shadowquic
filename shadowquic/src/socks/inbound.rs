use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc::channel;

use crate::config::{AuthUser, SocksServerCfg};
use crate::error::SError;
use crate::msgs::socks5::{
    self, AddrOrDomain, AuthReq, CmdReq, PasswordAuthReply, PasswordAuthReq,
    SOCKS5_AUTH_METHOD_NONE, SOCKS5_AUTH_METHOD_PASSWORD, SOCKS5_CMD_TCP_BIND,
    SOCKS5_CMD_TCP_CONNECT, SOCKS5_CMD_UDP_ASSOCIATE, SOCKS5_REPLY_SUCCEEDED, SOCKS5_VERSION,
};
use crate::msgs::{SDecode, SEncode, encode_to_async};
use crate::pool::FastSessionType;
use crate::pool::fast_create_session;
use crate::utils::dual_socket::to_ipv4_mapped;
use crate::{Inbound, ProxyRequest, TcpSession, UdpSession};
use async_trait::async_trait;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use anyhow::Result;
use tracing::{Instrument, trace, trace_span};

use super::UdpSocksWrap;

pub struct SocksServer {
    #[allow(dead_code)]
    bind_addr: SocketAddr,
    users: Vec<AuthUser>,
    listener: TcpListener,
}
impl SocksServer {
    pub async fn new(cfg: SocksServerCfg) -> Result<Self, SError> {
        let dual_stack = cfg.bind_addr.is_ipv6();
        let socket = Socket::new(
            // Use socket2 for dualstack for windows compact
            if dual_stack {
                Domain::IPV6
            } else {
                Domain::IPV4
            },
            Type::STREAM,
            Some(Protocol::TCP),
        )?;
        if dual_stack {
            let _ = socket
                .set_only_v6(false)
                .map_err(|e| tracing::warn!("failed to set dual stack for socket: {}", e));
        };
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&cfg.bind_addr.into())?;
        socket.listen(256)?;
        let listener = TcpListener::from_std(socket.into())
            .map_err(|e| SError::SocksError(format!("failed to create TcpListener: {e}")))?;
        Ok(Self {
            bind_addr: cfg.bind_addr,
            listener,
            users: cfg.users,
        })
    }
    pub async fn authenticate(&self, mut stream: TcpStream) -> Result<TcpStream, SError> {
        let auth_req = AuthReq::decode(&mut stream).await?;
        if auth_req.version != SOCKS5_VERSION {
            return Err(SError::ProtocolViolation);
        }
        let methods = auth_req.methods;
        if methods.contents.is_empty() {
            return Err(SError::ProtocolViolation);
        }
        let method = if self.users.is_empty() {
            SOCKS5_AUTH_METHOD_NONE
        } else {
            SOCKS5_AUTH_METHOD_PASSWORD
        };
        if !methods.contents.contains(&method) {
            return Err(SError::SocksError(format!(
                "authentication method not supported:{:?}",
                methods.contents
            )));
        }

        let reply = socks5::AuthReply {
            version: SOCKS5_VERSION,
            method,
        };
        encode_to_async(&reply, &mut stream).await?;
        if self.users.is_empty() {
            return Ok(stream);
        }
        let auth = PasswordAuthReq::decode(&mut stream).await?;
        if !self.users.contains(&AuthUser {
            username: String::from_utf8_lossy(&auth.username.contents).into_owned(),
            password: String::from_utf8_lossy(&auth.password.contents).into_owned(),
        }) {
            return Err(SError::SocksError("authentication failed".to_string()));
        }
        let reply = PasswordAuthReply {
            version: 0x01, // authentication version not socks version
            status: SOCKS5_REPLY_SUCCEEDED,
        };
        encode_to_async(&reply, &mut stream).await?;
        Ok(stream)
    }
    async fn handle_socks(
        &self,
        s: TcpStream,
        local_addr: SocketAddr,
    ) -> Result<(TcpStream, CmdReq, Option<UdpSocket>), SError> {
        let mut s = self.authenticate(s).await?;
        let req = socks5::CmdReq::decode(&mut s).await?;

        let addr = match req.dst.addr {
            AddrOrDomain::V4(_) | AddrOrDomain::Domain(_) => AddrOrDomain::V4([0u8, 0u8, 0u8, 0u8]),
            AddrOrDomain::V6(x) => AddrOrDomain::V6(x.map(|_| 0u8)),
        };

        let mut reply = socks5::CmdReply {
            version: SOCKS5_VERSION,
            rep: SOCKS5_REPLY_SUCCEEDED,
            rsv: 0u8,
            bind_addr: socks5::SocksAddr { addr, port: 0u16 },
        };
        let (reply, socket) = match req.cmd {
            SOCKS5_CMD_TCP_CONNECT => (reply, None),
            SOCKS5_CMD_UDP_ASSOCIATE => {
                let mut local_addr = local_addr;

                local_addr.set_port(0);
                let socket = UdpSocket::bind(local_addr).await?;
                let local_addr = socket.local_addr()?;
                reply.bind_addr = local_addr.into();
                (reply, Some(socket))
            }
            SOCKS5_CMD_TCP_BIND => {
                return Err(SError::ProtocolUnimpl);
            }
            _ => {
                return Err(SError::ProtocolViolation);
            }
        };

        encode_to_async(&reply, &mut s).await?;
        trace!("socks request accepted: {}", req.dst);
        Ok((s, req, socket))
    }
}

#[async_trait]
impl Inbound for SocksServer {
    async fn accept(&mut self) -> Result<ProxyRequest, SError> {
        let (stream, addr) = self.listener.accept().await?;
        let span = trace_span!("socks", src = addr.to_string());
        // ipv4 may be mapped for dual stack socket
        let local_addr = to_ipv4_mapped(stream.local_addr().unwrap());

        let (s, req, socket) = self
            .handle_socks(stream, local_addr)
            .instrument(span)
            .await?;

        // Create session tracking
        let remote_socket_addr = req
            .dst
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next());

        match req.cmd {
            SOCKS5_CMD_TCP_CONNECT => {
                let session_id = if let Some(raddr) = remote_socket_addr {
                    Some(fast_create_session(
                        raddr,
                        req.dst.clone(),
                        FastSessionType::Tcp,
                    ))
                } else {
                    None
                };
                Ok(ProxyRequest::Tcp(TcpSession {
                    stream: Box::new(s),
                    dst: req.dst,
                    session_id,
                }))
            }
            SOCKS5_CMD_UDP_ASSOCIATE => {
                let session_id = if let Some(raddr) = remote_socket_addr {
                    Some(fast_create_session(
                        raddr,
                        req.dst.clone(),
                        FastSessionType::UdpDatagram,
                    ))
                } else {
                    None
                };

                let socket = Arc::new(socket.unwrap());
                let (local_send, mut local_recv) = channel::<(Bytes, socks5::SocksAddr)>(512);
                let (local_send2, mut local_recv2) = channel::<(Bytes, socks5::SocksAddr)>(512);
                let socket1 = socket.clone();
                let socket2 = socket.clone();

                tokio::spawn(async move {
                    let mut wrap = UdpSocksWrap::new(socket1, true);
                    loop {
                        let (buf, addr) = match local_recv.recv().await {
                            Some(d) => d,
                            None => break,
                        };
                        let _ = wrap.send_to(buf, addr).await;
                    }
                });

                tokio::spawn(async move {
                    let mut wrap = UdpSocksWrap::new(socket2, true);
                    loop {
                        let result = wrap.recv_from().await;
                        match result {
                            Ok((buf, addr)) => {
                                if local_send2.send((buf, addr)).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });

                Ok(ProxyRequest::Udp(UdpSession {
                    send: Arc::new(local_send),
                    recv: Box::new(local_recv2),
                    bind_addr: req.dst,
                    stream: Some(Box::new(s)),
                    session_id,
                }))
            }
            _ => {
                return Err(SError::ProtocolViolation);
            }
        }
    }
}
