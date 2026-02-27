use std::net::ToSocketAddrs;
use std::sync::Arc;

use crate::{
    TcpSession, UdpRecv, UdpSend, UdpSession,
    msgs::socks5::{
        CmdReply, PasswordAuthReply, PasswordAuthReq, SOCKS5_AUTH_METHOD_PASSWORD,
        SOCKS5_CMD_TCP_CONNECT, SOCKS5_CMD_UDP_ASSOCIATE, SOCKS5_REPLY_SUCCEEDED, SOCKS5_RESERVE,
        SOCKS5_VERSION,
    },
    socks::UdpSocksWrap,
};
use tokio::{
    io::{AsyncReadExt, copy_bidirectional_with_sizes},
    net::{TcpStream, UdpSocket},
    sync::OnceCell,
};

use async_trait::async_trait;
use tracing::{Instrument, debug, error, trace_span};

use crate::{
    Outbound, ProxyRequest,
    config::SocksClientCfg,
    error::SError,
    msgs::socks5::{AuthReply, AuthReq, CmdReq, SOCKS5_AUTH_METHOD_NONE, VarVec},
    msgs::{SDecode, SEncode},
};

#[derive(Debug, Clone)]
pub struct SocksClient {
    pub addr: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[async_trait]
impl Outbound for SocksClient {
    async fn handle(&mut self, req: ProxyRequest) -> Result<(), SError> {
        let span = trace_span!("socks", server = self.addr);
        let client = self.clone();
        let fut = async move {
            match req {
                ProxyRequest::Tcp(tcp_session) => client.handle_tcp(tcp_session).await,
                ProxyRequest::Udp(udp_session) => client.handle_udp(udp_session).await,
            }
        };

        tokio::spawn(
            async {
                fut.await
                    .map_err(|x| error!("error due to handle socks request:{}", x))
            }
            .instrument(span),
        );
        Ok(())
    }
}

impl SocksClient {
    pub fn new(cfg: SocksClientCfg) -> Self {
        Self {
            addr: cfg.addr,
            username: cfg.username,
            password: cfg.password,
        }
    }

    async fn authenticate(&self, mut tcp: TcpStream) -> Result<TcpStream, SError> {
        let method = if self.username.is_some() {
            SOCKS5_AUTH_METHOD_PASSWORD
        } else {
            SOCKS5_AUTH_METHOD_NONE
        };
        let auth = AuthReq {
            version: SOCKS5_VERSION,
            methods: VarVec {
                len: 1,
                contents: vec![method],
            },
        };

        auth.encode(&mut tcp).await?;
        let rep = AuthReply::decode(&mut tcp).await?;
        if rep.version != SOCKS5_VERSION {
            return Err(SError::SocksError("version not supported".into()));
        }
        if rep.method != method {
            return Err(SError::SocksError(
                "authenticate method not supported".into(),
            ));
        }
        if let Some(username) = &self.username {
            let auth = PasswordAuthReq {
                version: 0x01, // This is password auth version not socks version
                username: VarVec {
                    len: username.len() as u8,
                    contents: username.as_bytes().to_vec(),
                },
                password: VarVec {
                    len: self.password.as_ref().unwrap().len() as u8,
                    contents: self
                        .password
                        .as_ref()
                        .ok_or(SError::SocksError("password not provided".into()))?
                        .as_bytes()
                        .to_vec(),
                },
            };
            auth.encode(&mut tcp).await?;
            let rep = PasswordAuthReply::decode(&mut tcp).await?;
            if rep.status != SOCKS5_REPLY_SUCCEEDED {
                return Err(SError::SocksError("authenticate failed".into()));
            }
        }
        Ok(tcp)
    }

    async fn handle_tcp(&self, mut tcp_session: TcpSession) -> Result<(), SError> {
        tracing::info!("connect to socks server: {}", self.addr);
        let tcp = TcpStream::connect(self.addr.clone()).await?;
        tcp.set_nodelay(true)?;
        let mut tcp = self.authenticate(tcp).await?;
        let socksreq = CmdReq {
            version: SOCKS5_VERSION,
            cmd: SOCKS5_CMD_TCP_CONNECT,
            rsv: SOCKS5_RESERVE,
            dst: tcp_session.dst,
        };
        socksreq.encode(&mut tcp).await?;
        let _rep = CmdReply::decode(&mut tcp).await?;
        tracing::trace!("socks tcp connection established");
        copy_bidirectional_with_sizes(&mut tcp, &mut tcp_session.stream, 16 * 1024, 16 * 1024)
            .await?;
        Ok(())
    }

    async fn handle_udp(&self, mut udp_session: UdpSession) -> Result<(), SError> {
        tracing::info!("connect to socks server: {}", self.addr);
        let tcp = TcpStream::connect(self.addr.clone()).await?;
        tcp.set_nodelay(true)?;

        let mut tcp = self.authenticate(tcp).await?;

        let socksreq = CmdReq {
            version: SOCKS5_VERSION,
            cmd: SOCKS5_CMD_UDP_ASSOCIATE,
            rsv: SOCKS5_RESERVE,
            dst: udp_session.bind_addr.clone(),
        };
        socksreq.encode(&mut tcp).await?;
        let rep = CmdReply::decode(&mut tcp).await?;
        tracing::trace!("socks udp association established");

        // 解析服务器返回的 UDP 中继地址
        let peer_addr = rep
            .bind_addr
            .to_socket_addrs()
            .expect("socks server return a unresolvable address")
            .next()
            .expect("socks server return a unresolvable address");

        // 获取 TCP 控制连接的远端 IP（即 Socks5 服务器 IP）
        let server_ip = tcp.peer_addr()?.ip();
        debug!("server IP: {}, original peer_addr: {}", server_ip, peer_addr);

        // 如果服务器返回通配 IP，则替换为服务器真实 IP；否则保持原样
        let target_ip = if peer_addr.ip().is_unspecified() {
            server_ip
        } else {
            peer_addr.ip()
        };
        let final_peer_addr = std::net::SocketAddr::new(target_ip, peer_addr.port());
        debug!("final UDP relay address: {}", final_peer_addr);

        let bind_addr = if final_peer_addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };

        let socket = UdpSocket::bind(bind_addr).await?;
        socket.connect(final_peer_addr).await?;
        let mut upstream =
            UdpSocksWrap(Arc::new(socket), OnceCell::new_with(Some(final_peer_addr)));

        let upstream_clone = upstream.clone();
        let fut1 = async move {
            loop {
                let (buf, dst) = upstream.recv_from().await?;
                debug!(
                    "RECV from upstream: {} bytes, first 8 bytes: {:02x?}, dst={}",
                    buf.len(),
                    &buf[..buf.len().min(8)],
                    dst
                );
                let _ = udp_session.send.send_to(buf, dst).await?;
            }
            #[allow(unreachable_code)]
            (Ok(()) as Result<(), SError>)
        };
        let fut2 = async move {
            loop {
                let (buf, dst) = udp_session.recv.recv_from().await?;
                debug!(
                    "SEND to upstream: {} bytes, first 8 bytes: {:02x?}, dst={}",
                    buf.len(),
                    &buf[..buf.len().min(8)],
                    dst
                );
                let _ = upstream_clone.send_to(buf, dst).await?;
            }
            #[allow(unreachable_code)]
            (Ok(()) as Result<(), SError>)
        };
        // control stream, in socks5 inbound, end of control stream
        // means end of udp association.
        let fut3 = async {
            if udp_session.stream.is_none() {
                return Ok(());
            }
            let mut buf = [0u8];
            // 使用 read 代替 read_exact，正确处理连接关闭
            match udp_session.stream.unwrap().read(&mut buf).await {
                Ok(0) => {
                    debug!("control stream closed by peer");
                    Ok(())
                }
                Ok(1) => {
                    // 收到数据，不符合规范
                    error!("unexpected data received from socks control stream");
                    Err(SError::UDPSessionClosed(
                        "unexpected data received from socks control stream".into(),
                    ))
                }
                Ok(n) => {
                    // 理论上不可能收到多于1字节，但以防万一
                    error!(
                        "unexpected data received from socks control stream ({} bytes)",
                        n
                    );
                    Err(SError::UDPSessionClosed(
                        "unexpected data received from socks control stream".into(),
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    debug!("control stream read unexpected eof");
                    Ok(())
                }
                Err(e) => Err(SError::UDPSessionClosed(format!(
                    "control stream error: {}",
                    e
                ))),
            }
        };
        // We can use spawn, but it requirs communication to shutdown the other
        // Flatten spawn handle using try_join! doesn't work. Don't know why
        tokio::try_join!(fut1, fut2, fut3)?;

        Ok(())
    }
}
