use std::{
    collections::hash_map::{self, Entry, HashMap},
    mem::replace,
    ops::Deref,
    sync::Arc,
    time::Duration,
};

use bytes::BytesMut;
use rustc_hash::{FxBuildHasher, FxHashMap};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{
        RwLock, SetOnce,
        watch::{Receiver, Sender, channel},
    },
};
use tracing::{Instrument, Level, debug, error, event, info, trace};

use crate::arena::{packet_buf_large, packet_buf_sized};
use crate::utils::lock_free_ring::LockFreeRingBuffer;
use crate::utils::memory_pool::{
    fast_alloc, fast_alloc_large, fast_alloc_medium, fast_alloc_small,
};

use crate::{
    AnyUdpRecv, AnyUdpSend,
    error::{SError, SResult},
    msgs::frame::{ClientHello, ConnectReq, Frame, ServerHello, UdpData, ERROR_OK, FEATURE_UDP},
    msgs::{
        SDecode, SDecodeSync, SEncode, SEncodeSync, decode_to_async, encode_to_async,
        socks5::SocksAddr,
    },
    quic::{QuicConnection, STATS},
};

mod id_store_optimized;
pub mod inbound;
pub mod outbound;

pub use id_store_optimized::LockFreeIdTable;
pub use self::inbound::SunnyQuicUsers;

/// SQuic connection, it is shared by sunnyquic and shadowquic and is a wrapper of quic connection.
/// It contains a connection object and two ID store for managing UDP sockets.
/// The IDStore stores the mapping between ids and the destination addresses as well as associated sockets
#[derive(Clone)]
pub struct SQConn<T: QuicConnection> {
    pub(crate) conn: T,
    pub(crate) authed: Arc<SetOnce<bool>>,
    pub(crate) auth_token: Arc<SetOnce<[u8; 64]>>,  // 0-RTT auth token
    pub(crate) send_id_store: IDStore<()>,
    pub(crate) recv_id_store: IDStore<(AnyUdpSend, SocksAddr)>,
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
                debug!("ServerHello received, connection ID: {}", server_hello.connection_id);
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

// Use watch channel here. Notify is not suitable here
// see https://github.com/tokio-rs/tokio/issues/3757
type IDStoreVal<T> = Result<T, Sender<()>>;

/// IDStore is a thread-safe store for managing UDP sockets and their associated ids.
/// It uses a HashMap to store the mapping between ids and the destination addresses as well as associated sockets.
/// It also uses an atomic counter to generate unique ids for new sockets.
#[derive(Clone)]
pub(crate) struct IDStore<T = (AnyUdpSend, SocksAddr)> {
    pub(crate) inner: Arc<RwLock<FxHashMap<u16, IDStoreVal<T>>>>,
}

impl<T> Default for IDStore<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::with_capacity_and_hasher(
                64,
                FxBuildHasher,
            ))),
        }
    }
}

impl<T> IDStore<T>
where
    T: Clone,
{
    async fn get_socket_or_notify(&self, id: u16) -> Result<T, Receiver<()>> {
        if let Some(r) = self.inner.read().await.get(&id) {
            r.clone().map_err(|x| x.subscribe())
        } else {
            match self.inner.write().await.entry(id) {
                Entry::Occupied(occupied_entry) => {
                    occupied_entry.get().clone().map_err(|x| x.subscribe())
                }
                Entry::Vacant(vacant_entry) => {
                    let (s, r) = channel(());
                    vacant_entry.insert(Err(s));
                    Err(r)
                }
            }
        }
    }
    async fn try_get_socket(&self, id: u16) -> Option<T> {
        if let Some(r) = self.inner.read().await.get(&id) {
            match r {
                Ok(s) => Some(s.clone()),
                Err(_) => None,
            }
        } else {
            None
        }
    }
    async fn get_socket_or_wait(&self, id: u16) -> Result<T, SError> {
        match self.get_socket_or_notify(id).await {
            Ok(r) => Ok(r),
            Err(mut n) => {
                n.changed()
                    .await
                    .map_err(|_| SError::UDPSessionClosed(String::from("notify sender dropped")))?;
                let ret = self
                    .try_get_socket(id)
                    .await
                    .ok_or(SError::UDPSessionClosed(String::from("UDP session closed")))?;
                Ok(ret)
            }
        }
    }
    async fn store_socket(&self, id: u16, val: T) {
        let mut h = self.inner.write().await;
        trace!("receiving side alive socket number: {}", h.len());
        let r = h.get_mut(&id);
        if let Some(s) = r {
            match s {
                Ok(_) => {
                    error!("id:{} already exists", id);
                }
                Err(_) => {
                    let notify = replace(s, Ok(val));
                    match notify {
                        Ok(_) => {
                            panic!("should be notify");
                        }
                        Err(n) => {
                            n.send(()).unwrap();
                            event!(Level::TRACE, "notify socket id:{}", id);
                        }
                    }
                }
            }
        } else {
            h.insert(id, Ok(val));
        }
    }
}

/// AssociateSendSession is a session for sending UDP packets.
/// It is created for each association task
/// The local dst_map works as a inverse map from destination to id
/// When session ended, the ids created by this session will be removed from the IDStore.
struct AssociateSendSession<W: AsyncWrite> {
    id_store: IDStore<()>,
    dst_map: FxHashMap<SocksAddr, u16>,
    unistream_map: FxHashMap<SocksAddr, W>,
}
impl<W: AsyncWrite> AssociateSendSession<W> {
    pub async fn get_id_or_insert(&mut self, addr: &SocksAddr) -> (u16, bool) {
        use std::collections::hash_map::Entry;
        match self.dst_map.entry(addr.clone()) {
            Entry::Occupied(e) => (*e.get(), false),
            Entry::Vacant(e) => {
                // Use simple counter for ID generation
                static COUNTER: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1);
                let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.id_store.inner.write().await.insert(id, Ok(()));
                e.insert(id);
                trace!("send session: insert id:{}, addr:{}", id, addr);
                (id, true)
            }
        }
    }
}

impl<W: AsyncWrite> Drop for AssociateSendSession<W> {
    fn drop(&mut self) {
        let id_store = self.id_store.inner.clone();
        let id_remove: Vec<u16> = self.dst_map.values().copied().collect();
        tokio::spawn(
            async move {
                let mut id_store = id_store.write().await;
                let len = id_store.len();
                for k in &id_remove {
                    id_store.remove(k);
                }
                let decrease = len - id_store.len();
                event!(
                    Level::TRACE,
                    "AssociateSendSession dropped, session id size:{}, {} ids cleaned",
                    id_remove.len(),
                    decrease
                );
            }
            .in_current_span(),
        );
    }
}

/// AssociateRecvSession is a session for receiving UDP ctrl stream.
/// It is created for each association task
/// There are two usages for id_map
/// First, it works as local cache avoiding using global store repeatedly which is more expensive
/// Second. it records ids created by this session and clean those ids when session ended.
struct AssociateRecvSession {
    id_store: IDStore<(AnyUdpSend, SocksAddr)>,
    id_map: FxHashMap<u16, SocksAddr>,
    lock_free_table: Arc<LockFreeIdTable>,
}
impl AssociateRecvSession {
    pub async fn store_socket(&mut self, id: u16, dst: SocksAddr, socks: AnyUdpSend) {
        if let hash_map::Entry::Vacant(e) = self.id_map.entry(id) {
            self.id_store
                .store_socket(id, (socks.clone(), dst.clone()))
                .await;
            // Also insert into lock-free table for fast path
            self.lock_free_table.insert(id, socks, dst.clone());
            trace!("recv session: insert id:{}, addr:{}", id, dst);
            e.insert(dst);
        }
    }
}

impl Drop for AssociateRecvSession {
    fn drop(&mut self) {
        let id_store = self.id_store.inner.clone();
        let id_remove: Vec<u16> = self.id_map.keys().copied().collect();
        tokio::spawn(
            async move {
                let mut id_store = id_store.write().await;
                let len = id_store.len();

                for k in &id_remove {
                    id_store.remove(k);
                }
                let decrease = len - id_store.len();
                event!(
                    Level::TRACE,
                    "AssociateRecvSession dropped, session id size:{}, {} ids cleaned",
                    id_remove.len(),
                    decrease
                );
            }
            .in_current_span(),
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
        id_store: conn.send_id_store.clone(),
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
        let (bytes, dst) = down_stream.recv_from().await?;
        STATS.packet_sent(bytes.len());

        let (id, is_new) = session.get_id_or_insert(&dst).await;

        // Send UDP data using new protocol
        if over_stream {
            use std::collections::hash_map::Entry;
            let uni_conn = match session.unistream_map.entry(dst.clone()) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let (uni, _id) = conn.open_uni().await?;
                    e.insert(uni)
                }
            };
            
            // Clear header buffer before encoding
            header_buf.clear();
            
            // Use new UdpData frame
            let udp_data = UdpData {
                dst: if is_new { Some(dst.clone()) } else { None },
                payload: bytes,
            };
            let mut frame = Frame::UdpData(udp_data);
            frame.encode_sync(&mut header_buf);
            uni_conn.write_all(&header_buf).await?;
        } else {
            // Datagram path - use new UdpData frame
            let mut frame_buf = BytesMut::new();
            
            let udp_data = UdpData {
                dst: if is_new { Some(dst.clone()) } else { None },
                payload: bytes,
            };
            Frame::UdpData(udp_data).encode_sync(&mut frame_buf);
            
            quic_conn
                .send_datagram(frame_buf.freeze())
                .await?;
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
    let mut session = AssociateRecvSession {
        id_store: conn.recv_id_store.clone(),
        id_map: HashMap::with_capacity_and_hasher(16, FxBuildHasher),
        lock_free_table: conn.lock_free_id_table.clone(),
    };
    loop {
        // Use new UdpData frame for control
        let frame = Frame::decode(&mut recv).await?;
        match frame {
            Frame::UdpData(udp_data) => {
                if let Some(dst) = udp_data.dst {
                    let id = rand::random::<u16>();
                    trace!("udp data received with dst: {}, assigning id:{}", dst, id);
                    session.store_socket(id, dst, udp_socket.clone()).await;
                }
            }
            Frame::Fin => {
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

/// Handle udp packet receive task - OPTIMIZED
/// Uses lock-free lookup first for maximum performance
pub async fn handle_udp_packet_recv<C: QuicConnection>(conn: SQConn<C>) -> Result<(), SError> {
    let id_store = conn.recv_id_store.clone();
    let lock_free_table = conn.lock_free_id_table.clone();

    wait_sunny_auth(&conn).await?;
    STATS.connection_opened();
    loop {
        tokio::select! {
            b = conn.read_datagram() => {
                let b = b?;
                STATS.packet_received(b.len());

                if b.len() < 2 {
                    continue;
                }

                // FAST PATH: Try lock-free lookup first (sync, no await)
                let id = u16::from_be_bytes([b[0], b[1]]);
                let payload = b.slice(2..);

                if let Some((udp, addr)) = lock_free_table.try_get(id) {
                    // Fast path: direct send without async lock
                    let _ = udp.send_to(payload, addr).await;
                    continue;
                }

                // SLOW PATH: Fall back to async HashMap lookup
                match id_store.get_socket_or_notify(id).await {
                 Ok((udp, addr)) =>  {
                     // Also insert into lock-free table for future fast path
                     lock_free_table.insert(id, udp.clone(), addr.clone());
                     udp.send_to(payload, addr).await?;
                 }
                 Err(mut notify) =>  {
                     let id_store = id_store.clone();
                     let lock_free_table = lock_free_table.clone();
                     let src_addr = conn.remote_address();
                     event!(Level::TRACE, "resolving datagram id:{}",id);
                     let payload = b;
                     tokio::spawn(async move {
                         let _ = notify.changed().await.map_err(|_|debug!("id:{} notifier dropped",id));
                         if let Some((udp, addr)) = id_store.try_get_socket(id).await {
                             // Insert into lock-free table for future fast path
                             lock_free_table.insert(id, udp.clone(), addr.clone());
                             info!("udp over datagram: id:{}: {}->{}",id, src_addr, addr);
                             let payload = payload.slice(2..);
                             let _ = udp.send_to(payload, addr).await.map_err(|x|error!("{}",x));
                         }
                         Ok(()) as Result<(), SError>
                      }.in_current_span());
                 }
            }
            }

            r = async {
                let (mut uni_stream, _id) = conn.accept_uni().await?;
                trace!("unistream accepted");
                
                // Use new UdpData frame
                let frame = Frame::decode(&mut uni_stream).await?;
                let (id, dst) = match frame {
                    Frame::UdpData(udp_data) => {
                        let id = rand::random::<u16>();
                        (id, udp_data.dst.unwrap_or_else(|| SocksAddr::from_domain("0.0.0.0".to_string(), 0)))
                    }
                    Frame::Fin => {
                        return Ok((uni_stream, None, None)) as Result<_, SError>;
                    }
                    _ => return Err(SError::ProtocolViolation),
                };
                
                event!(Level::TRACE, "resolving datagram id:{}",id);

                let (udp,addr) = id_store.get_socket_or_wait(id).await?;

                info!("udp over stream: id:{}: {}->{}",id, conn.remote_address(), addr);
                Ok((uni_stream,Some(udp.clone()),Some(addr.clone()))) as Result<(C::RecvStream,Option<AnyUdpSend>,Option<SocksAddr>),SError>
            } => {

                let  (mut uni_stream,udp,addr) = match r {
                    Ok(r) => r,
                    Err(SError::UDPSessionClosed(_)) => {
                        continue;
                    }
                    Err(e) => {
                        return Err(e);
                    }
                };

                // Handle Fin frame
                if udp.is_none() {
                    trace!("UDP stream finished");
                    continue;
                }

                let udp = udp.unwrap();
                let addr = addr.unwrap();

                // Batch receive and send for better throughput
                tokio::spawn(async move {
                    loop {
                        // Use new UdpData frame for receiving
                        let frame = match Frame::decode(&mut uni_stream).await {
                            Ok(f) => f,
                            Err(_) => break,
                        };
                        
                        match frame {
                            Frame::UdpData(udp_data) => {
                                let payload = udp_data.payload;
                                let _ = udp.send_to(payload, addr.clone()).await;
                            }
                            Frame::Fin => {
                                trace!("UDP stream finished");
                                break;
                            }
                            _ => break,
                        }
                    }
                    Ok(()) as Result<(), SError>
                }.in_current_span());
            }
        }
    }
    #[allow(unreachable_code)]
    Ok(())
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
