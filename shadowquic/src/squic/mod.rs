use std::{
    collections::hash_map::{self, Entry, HashMap},
    mem::replace,
    ops::Deref,
    sync::Arc,
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use rustc_hash::{FxBuildHasher, FxHashMap};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{
        SetOnce,
        watch::{Receiver, Sender, channel},
    },
};
use tracing::{Instrument, Level, debug, error, event, info, trace, warn};

use crate::arena::{packet_buf_large, packet_buf_sized};
use crate::utils::lock_free_ring::LockFreeRingBuffer;
use crate::utils::memory_pool::{
    fast_alloc, fast_alloc_large, fast_alloc_medium, fast_alloc_small,
};

use crate::{
    AnyUdpRecv, AnyUdpSend, ProxyRequest, UdpRecv, UdpSend, UdpSession,
    error::{SError, SResult},
    msgs::frame::{ClientHello, ConnectReq, ERROR_OK, FEATURE_UDP, Frame, ServerHello, UdpData},
    msgs::{
        SDecode, SDecodeSync, SEncode, SEncodeSync, decode_to_async, encode_to_async,
        socks5::SocksAddr,
    },
    quic::{QuicConnection, STATS},
};

pub mod id_store_optimized;
pub mod inbound;
pub mod outbound;

pub use self::inbound::SunnyQuicUsers;
pub use id_store_optimized::{LockFreeIdTable, UdpIdStore};

/// SQuic connection, it is shared by sunnyquic and shadowquic and is a wrapper of quic connection.
/// It contains a connection object and two ID store for managing UDP sockets.
/// Now uses lock-free UdpIdStore instead of lock-based IDStore for better performance
#[derive(Clone)]
pub struct SQConn<T: QuicConnection> {
    pub(crate) conn: T,
    pub(crate) authed: Arc<SetOnce<bool>>,
    pub(crate) auth_token: Arc<SetOnce<[u8; 64]>>, // 0-RTT auth token
    pub(crate) send_id_counter: Arc<AtomicU16>,    // Lock-free ID counter
    pub(crate) recv_id_store: Arc<UdpIdStore>,     // Lock-free UDP socket store
    pub(crate) lock_free_id_table: Arc<LockFreeIdTable>,
}

async fn wait_sunny_auth<T: QuicConnection>(conn: &SQConn<T>) -> SResult<()> {
    match tokio::time::timeout(Duration::from_millis(3200), conn.authed.wait()).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(SError::SunnyAuthError("Wrong psassword/username".into())),
        Err(_) => Err(SError::SunnyAuthError("timeout".into())),
    }
}

/// Type alias for authentication credential
pub type SunnyCredential = [u8; 64];

pub(crate) async fn auth_sunny<T: QuicConnection>(
    conn: &SQConn<T>,
    user_hash: SunnyCredential,
) -> SResult<()> {
    if conn.authed.get().is_none() {
        let (mut send, mut recv, _id) = conn.open_bi().await?;

        // 发送ClientHello进行握手
        let hello = ClientHello {
            version: 1,
            supported_features: FEATURE_UDP,
            auth_token: user_hash,
            extensions: vec![],
        };
        Frame::ClientHello(hello).encode(&mut send).await?;
        debug!("ClientHello sent");

        // 接收ServerHello
        let frame = Frame::decode(&mut recv).await?;
        match frame {
            Frame::ServerHello(server_hello) => {
                debug!(
                    "ServerHello received, connection ID: {}",
                    server_hello.connection_id
                );
                if server_hello.version != 1 {
                    return Err(SError::ProtocolViolation);
                }
            }
            _ => {
                conn.close(263, &[]);
                return Err(SError::SunnyAuthError("Protocol error".into()));
            }
        }

        conn.authed.set(true).expect("repeated authentication");
    }
    Ok(())
}

impl<T: QuicConnection> Deref for SQConn<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

/// AssociateSendSession is a session for sending UDP packets.
/// It is created for each association task
/// The local dst_map works as a inverse map from destination to id
/// Now uses lock-free AtomicU16 counter instead of lock-based IDStore
struct AssociateSendSession<W: AsyncWrite> {
    id_counter: Arc<AtomicU16>,
    dst_map: FxHashMap<SocksAddr, u16>,
    unistream_map: FxHashMap<SocksAddr, W>,
}
impl<W: AsyncWrite> AssociateSendSession<W> {
    pub async fn get_id_or_insert(&mut self, addr: &SocksAddr) -> (u16, bool) {
        use std::collections::hash_map::Entry;
        match self.dst_map.entry(addr.clone()) {
            Entry::Occupied(e) => (*e.get(), false),
            Entry::Vacant(e) => {
                let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
                e.insert(id);
                trace!("send session: insert id:{}, addr:{}", id, addr);
                (id, true)
            }
        }
    }
}

impl<W: AsyncWrite> Drop for AssociateSendSession<W> {
    fn drop(&mut self) {
        // No global cleanup needed - IDs are tracked locally in dst_map
        trace!(
            "AssociateSendSession dropped, {} ids cleaned",
            self.dst_map.len()
        );
    }
}

/// AssociateRecvSession is a session for receiving UDP ctrl stream.
/// It is created for each association task
/// Now uses lock-free UdpIdStore instead of lock-based IDStore
struct AssociateRecvSession {
    id_store: Arc<UdpIdStore>,
    id_map: FxHashMap<u16, SocksAddr>,
    lock_free_table: Arc<LockFreeIdTable>,
}
impl AssociateRecvSession {
    pub async fn store_socket(&mut self, id: u16, dst: SocksAddr, socks: AnyUdpSend) {
        if let hash_map::Entry::Vacant(e) = self.id_map.entry(id) {
            // Clone dst once for both stores
            let dst_for_table = dst.clone();
            // Use lock-free UdpIdStore
            let _ = self.id_store.insert(id, socks.clone(), dst.clone());
            // Also insert into lock-free table for fast path
            self.lock_free_table.insert(id, socks, dst_for_table);
            trace!("recv session: insert id:{}, addr:{}", id, dst);
            e.insert(dst);
        }
    }
}

impl Drop for AssociateRecvSession {
    fn drop(&mut self) {
        // Clean up using lock-free table
        for id in self.id_map.keys() {
            self.id_store.remove(*id);
            self.lock_free_table.remove(*id);
        }
        trace!(
            "AssociateRecvSession dropped, {} ids cleaned",
            self.id_map.len()
        );
    }
}

/// Handle udp packets send - OPTIMIZED
/// Uses pre-allocated buffers and reduces copies
pub async fn handle_udp_send<C: QuicConnection>(
    mut send: C::SendStream,
    udp_recv: AnyUdpRecv,
    conn: SQConn<C>,
    over_stream: bool,
) -> Result<(), SError> {
    let mut down_stream = udp_recv;
    let mut session = AssociateSendSession {
        id_counter: conn.send_id_counter.clone(),
        dst_map: HashMap::with_capacity_and_hasher(16, FxBuildHasher),
        unistream_map: HashMap::with_capacity_and_hasher(16, FxBuildHasher),
    };
    let quic_conn = conn.conn.clone();
    STATS.connection_opened();

    // Pre-allocated buffers to avoid per-packet allocation
    let mut header_buf = BytesMut::with_capacity(64);
    // Stack buffer for small packets (avoid heap allocation)
    let mut stack_buf = [0u8; 256];

    loop {
        trace!("handle_udp_send: waiting for data from downstream...");
        let (bytes, dst) = down_stream.recv_from().await?;
        trace!(
            "handle_udp_send: got {} bytes to {}, sending to upstream...",
            bytes.len(),
            dst
        );
        STATS.packet_sent(bytes.len());

        let (id, is_new) = session.get_id_or_insert(&dst).await;

        // Send UDP data using new protocol
        if over_stream {
            trace!("handle_udp_send: using stream mode, is_new: {}", is_new);
            use std::collections::hash_map::Entry;
            let uni_conn = match session.unistream_map.entry(dst.clone()) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    trace!("handle_udp_send: opening new unidirectional stream");
                    let (uni, _id) = conn.open_uni().await?;
                    trace!("handle_udp_send: unidirectional stream opened");
                    e.insert(uni)
                }
            };

            // Clear header buffer before encoding
            header_buf.clear();

            // Use new UdpData frame
            // Always include destination to ensure server can route packets correctly
            let udp_data = UdpData {
                dst: Some(dst.clone()),
                payload: bytes,
            };
            let mut frame = Frame::UdpData(udp_data);
            frame.encode_sync(&mut header_buf);
            trace!(
                "handle_udp_send: writing {} bytes to unidirectional stream (stream mode)",
                header_buf.len()
            );
            trace!(
                "handle_udp_send: QUIC connection status: close_reason={:?}",
                conn.close_reason()
            );
            uni_conn.write_all(&header_buf).await?;
            trace!("handle_udp_send: data written to unidirectional stream successfully");
        } else {
            // Datagram path - use new UdpData frame
            let mut frame_buf = BytesMut::new();

            // Always include destination to ensure server can route packets correctly
            let udp_data = UdpData {
                dst: Some(dst.clone()),
                payload: bytes,
            };
            Frame::UdpData(udp_data).encode_sync(&mut frame_buf);
            trace!(
                "handle_udp_send: sending {} bytes via datagram",
                frame_buf.len()
            );

            quic_conn.send_datagram(frame_buf.freeze()).await?;
            trace!("handle_udp_send: datagram sent");
        }
    }
}

/// Handle udp ctrl stream receive task
/// it retrieves the dst id pair from the bistream and records related socket and address
/// This function is symetrical for both clients and servers.
pub async fn handle_udp_recv_ctrl<C: QuicConnection>(
    mut recv: C::RecvStream,
    udp_socket: AnyUdpSend,
    conn: SQConn<C>,
) -> Result<(), SError> {
    trace!("handle_udp_recv_ctrl: started, waiting for upstream data...");
    let mut session = AssociateRecvSession {
        id_store: conn.recv_id_store.clone(),
        id_map: HashMap::with_capacity_and_hasher(16, FxBuildHasher),
        lock_free_table: conn.lock_free_id_table.clone(),
    };
    loop {
        trace!("handle_udp_recv_ctrl: waiting for frame from upstream...");
        let frame =
            tokio::time::timeout(std::time::Duration::from_secs(5), Frame::decode(&mut recv)).await;
        let frame = match frame {
            Ok(Ok(f)) => {
                trace!("handle_udp_recv_ctrl: got frame");
                f
            }
            Ok(Err(e)) => {
                error!("handle_udp_recv_ctrl: frame decode error: {}", e);
                break;
            }
            Err(_) => {
                trace!("handle_udp_recv_ctrl: timeout waiting for frame, continuing...");
                continue;
            }
        };
        trace!(
            "handle_udp_recv_ctrl: got frame: {:?}",
            std::mem::discriminant(&frame)
        );
        match frame {
            Frame::UdpData(udp_data) => {
                let dst = udp_data.dst.clone();

                if let Some(ref dst) = dst {
                    let id = rand::random::<u16>();
                    trace!(
                        "handle_udp_recv_ctrl: received UDP data, dst: {}, id: {}",
                        dst, id
                    );
                    trace!("udp data received with dst: {}, assigning id:{}", dst, id);
                    session
                        .store_socket(id, dst.clone(), udp_socket.clone())
                        .await;

                    // 发送UDP数据到channel (UdpSession.recv)
                    // dst是目标地址，payload是数据
                    trace!(
                        "handle_udp_recv_ctrl: sending {} bytes to channel, dst: {}",
                        udp_data.payload.len(),
                        dst
                    );
                    let _ = udp_socket.send_to(udp_data.payload, dst.clone()).await;
                    trace!("handle_udp_recv_ctrl: sent to channel");
                } else {
                    // 没有目标地址的UDP数据（不应该出现）
                    warn!("handle_udp_recv_ctrl: UDP data received without dst!");
                    trace!("udp data received without dst");
                }
            }
            Frame::Fin => {
                trace!("handle_udp_recv_ctrl: received Fin frame, closing");
                trace!("UDP session finished");
                break;
            }
            _ => {
                trace!("unexpected frame type in UDP control");
            }
        }
    }
    #[allow(unreachable_code)]
    Ok(())
}

/// Hash a destination address to get a consistent ID for session lookup
pub fn hash_dst(dst: &SocksAddr) -> u16 {
    use rustc_hash::FxHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = FxHasher::default();
    dst.hash(&mut hasher);
    (hasher.finish() & 0xFFFF) as u16
}

/// Handle UDP relay from client to DirectOut (server-side)
/// This handles the data path: client -> uni stream -> DirectOut
/// Looks up the existing UDP session from lock_free_id_table and forwards data through it
pub async fn handle_udp_relay_to_outbound<C: QuicConnection>(
    conn: SQConn<C>,
    req_send: tokio::sync::mpsc::Sender<ProxyRequest>,
) -> Result<(), SError> {
    trace!("handle_udp_relay_to_outbound: waiting for auth...");
    wait_sunny_auth(&conn).await?;
    trace!("handle_udp_relay_to_outbound: auth passed, entering main loop");

    let lock_free_table = conn.lock_free_id_table.clone();

    loop {
        let (mut uni_stream, _id) = conn.accept_uni().await?;
        trace!("handle_udp_relay_to_outbound: accepted uni stream, decoding frame");

        let frame = Frame::decode(&mut uni_stream).await?;
        let first_udp_data = match frame {
            Frame::UdpData(udp_data) => udp_data,
            Frame::Fin => {
                trace!("UDP stream finished");
                continue;
            }
            _ => return Err(SError::ProtocolViolation),
        };
        let dst = first_udp_data
            .dst
            .clone()
            .unwrap_or_else(|| SocksAddr::from_domain("0.0.0.0".to_string(), 0));

        trace!("handle_udp_relay_to_outbound: UDP to {}", dst);

        // Look up the existing UDP session from lock_free_id_table
        // Use wildcard ID 0 for UDP ASSOCIATE with unspecified destination
        let id = 0u16;

        // Try to get the existing session, with retry for race condition
        // (UDP data may arrive before UDP ASSOCIATE is processed)
        let mut found = None;
        for _ in 0..20 {
            if let Some((udp_send, _addr)) = lock_free_table.try_get(id) {
                found = Some(udp_send.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let Some(udp_send_clone) = found else {
            warn!(
                "handle_udp_relay_to_outbound: no UDP session found for id {}",
                id
            );
            continue;
        };

        // Send the first UDP packet
        trace!(
            "Forwarding UDP to outbound: {} bytes to {}",
            first_udp_data.payload.len(),
            dst
        );
        let _ = udp_send_clone
            .send_to(first_udp_data.payload, dst.clone())
            .await;

        // Process remaining frames in the stream
        tokio::spawn(async move {
            loop {
                let frame = match Frame::decode(&mut uni_stream).await {
                    Ok(f) => f,
                    Err(_) => break,
                };

                match frame {
                    Frame::UdpData(udp_data) => {
                        let payload = udp_data.payload;
                        let pkt_dst = udp_data.dst.unwrap_or_else(|| dst.clone());
                        trace!(
                            "Forwarding UDP to outbound: {} bytes to {}",
                            payload.len(),
                            pkt_dst
                        );
                        let _ = udp_send_clone.send_to(payload, pkt_dst).await;
                    }
                    Frame::Fin => {
                        trace!("UDP stream finished");
                        break;
                    }
                    _ => break,
                }
            }
            Ok(()) as Result<(), SError>
        });
    }
}

/// Handle UDP responses from server (client-side)
/// This handles the data path: server -> uni stream -> client UDP socket
pub async fn handle_udp_from_server<C: QuicConnection>(
    conn: SQConn<C>,
    udp_socket: AnyUdpSend,
) -> Result<(), SError> {
    trace!("handle_udp_from_server: waiting for auth...");
    wait_sunny_auth(&conn).await?;
    trace!("handle_udp_from_server: auth passed, entering main loop");

    loop {
        let (mut uni_stream, _id) = conn.accept_uni().await?;
        trace!("handle_udp_from_server: accepted uni stream, decoding frame");

        let udp_socket_clone = udp_socket.clone();
        tokio::spawn(async move {
            loop {
                let frame = match Frame::decode(&mut uni_stream).await {
                    Ok(f) => f,
                    Err(_) => break,
                };

                match frame {
                    Frame::UdpData(udp_data) => {
                        let payload = udp_data.payload;
                        let dst = udp_data
                            .dst
                            .unwrap_or_else(|| SocksAddr::from_domain("0.0.0.0".to_string(), 0));
                        trace!(
                            "Client: received UDP from server: {} bytes to {}",
                            payload.len(),
                            dst
                        );
                        let _ = udp_socket_clone.send_to(payload, dst).await;
                    }
                    Frame::Fin => {
                        trace!("UDP stream finished");
                        break;
                    }
                    _ => break,
                }
            }
            Ok(()) as Result<(), SError>
        });
    }
}

/// Optimized batch packet processor using LockFreeRingBuffer
pub struct BatchPacketProcessor {
    ring: Arc<LockFreeRingBuffer>,
    pending: std::collections::HashMap<u16, (AnyUdpSend, SocksAddr)>,
}

impl BatchPacketProcessor {
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: Arc::new(LockFreeRingBuffer::new(capacity)),
            pending: std::collections::HashMap::with_capacity(64),
        }
    }

    #[inline]
    pub fn push_packet(
        &self,
        id: u16,
        payload: bytes::Bytes,
        addr: SocksAddr,
        udp: Option<(AnyUdpSend, SocksAddr)>,
    ) -> bool {
        if let Some((_sender, dst)) = udp {
            let entry = crate::utils::lock_free_ring::RingEntry {
                data: payload,
                src_addr: "0.0.0.0:0".parse().unwrap(),
                dst_addr: format!("{}:{}", dst.addr, dst.port)
                    .parse()
                    .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap()),
            };
            self.ring.push(entry)
        } else {
            false
        }
    }

    #[inline]
    pub fn pop_packet(&self) -> Option<crate::utils::lock_free_ring::RingEntry> {
        self.ring.pop()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}
