use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::{Duration, timeout};

use tokio::io::AsyncReadExt;
use tracing::Instrument;
use tracing::{Level, debug, error, info, span, trace};

use crate::{
    ProxyRequest,
    error::SError,
    msgs::frame::{ConnectReq, FastConnectReq, Frame, UdpAssociateReq},
    msgs::{SDecode, SEncode, encode_to_async, socks5::SocksAddr},
    quic::QuicConnection,
    squic::{handle_udp_from_server, handle_udp_recv_ctrl, handle_udp_send},
};

use super::{SQConn, inbound::Unsplit};
/// Handling a proxy request and starting proxy task with given squic connection
pub async fn handle_request<C: QuicConnection>(
    req: ProxyRequest,
    conn: SQConn<C>,
    over_stream: bool,
) -> Result<(), SError> {
    let (mut send, mut recv, id) = QuicConnection::open_bi(&conn.conn).await?;
    let _span = span!(Level::TRACE, "bistream", id = id);
    let fut = async move {
        // 获取认证信息（如果有的话）
        let auth_token = conn.auth_token.get().cloned();

        match req {
            crate::ProxyRequest::Tcp(mut tcp_session) => {
                let dst = tcp_session.dst.clone();
                debug!("bistream opened for tcp dst:{}", dst);

                // 0-RTT: 使用FastConnect合并认证+连接
                if let Some(token) = auth_token {
                    let req = FastConnectReq {
                        auth_token: token,
                        dst: tcp_session.dst.clone(),
                        extensions: vec![],
                    };
                    Frame::FastConnect(req).encode(&mut send).await?;
                    trace!("FastConnect (0-RTT) sent");

                    // 等待FastConnectAck响应
                    let ack = Frame::decode(&mut recv).await?;
                    trace!("FastConnectAck received: {:?}", ack);
                } else {
                    // 1-RTT: 普通Connect（无认证）
                    let req = ConnectReq {
                        dst: tcp_session.dst,
                        extensions: vec![],
                    };
                    Frame::Connect(req).encode(&mut send).await?;
                    trace!("tcp connect req header sent");

                    // 尝试非阻塞读取并丢弃可能的ConnectAck
                    // Server可能不发送ConnectAck，超时则忽略
                    match timeout(Duration::from_millis(0), Frame::decode(&mut recv)).await {
                        Ok(Ok(Frame::ConnectAck(_))) => {
                            trace!("ConnectAck discarded");
                        }
                        _ => {}
                    }
                }

                // 直接开始数据传输
                use crate::utils::memory_pool::fast_alloc_large;
                let mut buf_quic = fast_alloc_large();
                let mut buf_tcp = fast_alloc_large();

                let (down, up) = tokio::io::copy_bidirectional_with_sizes(
                    &mut Unsplit { s: send, r: recv },
                    &mut tcp_session.stream,
                    buf_quic.capacity(),
                    buf_tcp.capacity(),
                )
                .await?;
                info!(
                    "request:{} finished, upload:{}bytes,download:{}bytes",
                    dst, up, down
                );
            }
            crate::ProxyRequest::Udp(udp_session) => {
                let bind_addr = udp_session.bind_addr.clone();
                debug!("ShadowQUIC outbound: UDP request, bind_addr: {}", bind_addr);
                debug!("bistream opened for udp dst:{}", bind_addr);
                let req = UdpAssociateReq {
                    dst: Some(udp_session.bind_addr),
                    extensions: vec![],
                };
                debug!("Sending UDP ASSOCIATE request to upstream...");
                Frame::UdpAssociate(req).encode(&mut send).await?;
                debug!("UDP ASSOCIATE request sent to upstream");

                // Clone UDP session components before moving into async blocks
                let udp_send_clone = udp_session.send.clone();
                let udp_recv_for_send = udp_session.recv;

                // Spawn handle_udp_recv_ctrl as background task - it reads from control stream
                // which may close after UDP ASSOCIATE exchange
                let conn_for_ctrl = conn.clone();
                tokio::spawn(async move {
                    trace!("Client: starting handle_udp_recv_ctrl task...");
                    let result = handle_udp_recv_ctrl(recv, udp_send_clone, conn_for_ctrl).await;
                    trace!("Client: handle_udp_recv_ctrl finished: {:?}", result);
                });

                // Spawn handle_udp_from_server to receive UDP responses from server via uni streams
                let conn_for_server = conn.clone();
                let udp_send_for_response = udp_session.send.clone();
                tokio::spawn(async move {
                    trace!("Client: starting handle_udp_from_server task...");
                    let result =
                        handle_udp_from_server(conn_for_server, udp_send_for_response).await;
                    trace!("Client: handle_udp_from_server finished: {:?}", result);
                });

                // Main task: handle_udp_send - send UDP data to upstream
                let conn_for_send = conn.clone();
                let fut1 = handle_udp_send(send, udp_recv_for_send, conn_for_send, over_stream);

                // Control stream monitoring - spawn instead of join
                let conn_for_ctrl2 = conn.clone();
                let mut ctrl_stream = udp_session.stream;
                tokio::spawn(async move {
                    if ctrl_stream.is_none() {
                        return;
                    }
                    trace!("Client: monitoring control stream...");
                    let mut buf = [0u8];
                    match ctrl_stream.unwrap().read_exact(&mut buf).await {
                        Ok(_) => trace!("Client: control stream closed normally"),
                        Err(e) => trace!("Client: control stream error: {}", e),
                    }
                });

                tokio::try_join!(fut1)?;
                debug!("udp association to {} ended", bind_addr);
            }
        }
        Ok(()) as Result<(), SError>
    };
    tokio::spawn(async {
        let _ = fut.instrument(_span).await.map_err(|x| error!("{}", x));
    });
    Ok(())
}

/// Helper function to create new stream for proxy dstination
#[allow(dead_code)]
pub async fn connect_tcp<C: QuicConnection>(
    sq_conn: &SQConn<C>,
    dst: SocksAddr,
) -> Result<Unsplit<C::SendStream, C::RecvStream>, crate::error::SError> {
    let conn = sq_conn;

    let (mut send, recv, _id) = conn.open_bi().await?;

    info!("bistream opened for tcp dst:{}", dst);
    //let _enter = _span.enter();
    let req = ConnectReq {
        dst,
        extensions: vec![],
    };
    Frame::Connect(req).encode(&mut send).await?;
    trace!("req header sent");

    Ok(Unsplit { s: send, r: recv })
}

/// associate a udp socket in the remote server
/// return a socket-like send, recv handle.
#[allow(dead_code)]
pub async fn associate_udp<C: QuicConnection>(
    sq_conn: &SQConn<C>,
    dst: SocksAddr,
    over_stream: bool,
) -> Result<(Sender<(Bytes, SocksAddr)>, Receiver<(Bytes, SocksAddr)>), SError> {
    let conn = sq_conn;

    let (mut send, recv, _id) = conn.open_bi().await?;

    info!("bistream opened for udp dst:{}", dst);

    let req = UdpAssociateReq {
        dst: Some(dst),
        extensions: vec![],
    };
    Frame::UdpAssociate(req).encode(&mut send).await?;
    let (local_send, udp_recv) = channel::<(Bytes, SocksAddr)>(256);
    let (udp_send, local_recv) = channel::<(Bytes, SocksAddr)>(256);
    let local_send = Arc::new(local_send);
    let fut2 = handle_udp_recv_ctrl(recv, local_send, conn.clone());
    let fut1 = handle_udp_send(send, Box::new(local_recv), conn.clone(), over_stream);

    tokio::spawn(async {
        match tokio::try_join!(fut1, fut2) {
            Err(e) => error!("udp association ended due to {}", e),
            Ok(_) => trace!("udp association ended"),
        }
    });

    Ok((udp_send, udp_recv))
}
