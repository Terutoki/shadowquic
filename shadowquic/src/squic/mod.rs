//! This module is shared by sunnyquic and shadowquic
//! It handles the general tcp/udp proxying logic over quic connection
//! It contains an optional authentication feature for sunnyquic only

use std::{
    collections::hash_map::{self, Entry},
    mem::replace,
    ops::Deref,
    sync::{Arc, atomic::AtomicU16},
    time::Duration,
};

use ahash::AHashMap;
use bytes::BytesMut;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{
        RwLock, SetOnce,
        watch::{Receiver, Sender, channel},
    },
};
use tracing::{Instrument, Level, debug, error, event, info, trace};

use crate::{
    AnyUdpRecv, AnyUdpSend,
    error::{SError, SResult},
    msgs::squic::SunnyCredential,
    msgs::{
        SDecode, SEncode,
        socks5::SocksAddr,
        squic::{SQPacketDatagramHeader, SQReq, SQUdpControlHeader},
    },
    quic::{QuicConnection, STATS},
};

pub mod inbound;
pub mod outbound;

/// SQuic connection, it is shared by shadowquic and sunnyquic and is a wrapper of quic connection.
/// It contains a connection object and two ID store for managing UDP sockets.
/// The IDStore stores the mapping between ids and the destionation addresses as well as associated sockets
#[derive(Clone)]
pub struct SQConn<T: QuicConnection> {
    pub(crate) conn: T,
    pub(crate) authed: Arc<SetOnce<bool>>,
    pub(crate) send_id_store: IDStore<()>,
    pub(crate) recv_id_store: IDStore<(AnyUdpSend, SocksAddr)>,
}

async fn wait_sunny_auth<T: QuicConnection>(conn: &SQConn<T>) -> SResult<()> {
    match tokio::time::timeout(Duration::from_millis(3200), conn.authed.wait()).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(SError::SunnyAuthError("Wrong psassword/username".into())),
        Err(_) => Err(SError::SunnyAuthError("timeout".into())),
    }
}

pub(crate) async fn auth_sunny<T: QuicConnection>(
    conn: &SQConn<T>,
    user_hash: SunnyCredential,
) -> SResult<()> {
    if conn.authed.get().is_none() {
        let (mut send, _recv, _id) = conn.open_bi().await?;
        SQReq::SQAuthenticate(user_hash).encode(&mut send).await?;
        debug!("authentication request sent");
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
#[derive(Clone, Default)]
pub(crate) struct IDStore<T = (AnyUdpSend, SocksAddr)> {
    pub(crate) id_counter: Arc<AtomicU16>,
    pub(crate) inner: Arc<RwLock<AHashMap<u16, IDStoreVal<T>>>>,
}

impl<T> IDStore<T>
where
    T: Clone,
{
    async fn get_socket_or_notify(&self, id: u16) -> Result<T, Receiver<()>> {
        if let Some(r) = self.inner.read().await.get(&id) {
            r.clone().map_err(|x| x.subscribe())
        } else {
            // Need to recheck
            // During change from read lock to write lock, hashmap may be modified
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
                // This may fail is UDP session is closed right at this moment.
                n.changed()
                    .await
                    .map_err(|_| SError::UDPSessionClosed(String::from("notify sender dropped")))?;
                //
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
                    //let _ = notify.map_err(|x| x.notify_one());
                    match notify {
                        Ok(_) => {
                            panic!("should be notify"); // should never happen
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
    async fn fetch_new_id(&self, val: T) -> u16 {
        let mut inner = self.inner.write().await;
        trace!("sending side socket number: {}", inner.len());
        let mut r;
        loop {
            r = self
                .id_counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst); // Wrapping occured if overflow
            if let Entry::Vacant(e) = inner.entry(r) {
                e.insert(Ok(val));
                break;
            }
        }
        r
    }
}

/// AssociateSendSession is a session for sending UDP packets.
/// It is created for each association task
/// The local dst_map works as a inverse map from destination to id
/// When session ended, the ids created by this session will be removed from the IDStore.
struct AssociateSendSession<W: AsyncWrite> {
    id_store: IDStore<()>,
    dst_map: AHashMap<SocksAddr, u16>,
    unistream_map: AHashMap<SocksAddr, W>,
}
impl<W: AsyncWrite> AssociateSendSession<W> {
    pub async fn get_id_or_insert(&mut self, addr: &SocksAddr) -> (u16, bool) {
        use std::collections::hash_map::Entry;
        match self.dst_map.entry(addr.clone()) {
            Entry::Occupied(e) => (*e.get(), false),
            Entry::Vacant(e) => {
                let id = self.id_store.fetch_new_id(()).await;
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
    id_map: AHashMap<u16, SocksAddr>,
}
impl AssociateRecvSession {
    pub async fn store_socket(&mut self, id: u16, dst: SocksAddr, socks: AnyUdpSend) {
        if let hash_map::Entry::Vacant(e) = self.id_map.entry(id) {
            self.id_store.store_socket(id, (socks, dst.clone())).await;
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

/// Handle udp packets send
/// It watches the udp socket and sends the packets to the quic connection.
/// This function is symetrical for both clients and servers.
pub async fn handle_udp_send<C: QuicConnection>(
    mut send: C::SendStream,
    udp_recv: AnyUdpRecv,
    conn: SQConn<C>,
    over_stream: bool,
) -> Result<(), SError> {
    let mut down_stream = udp_recv;
    let mut session = AssociateSendSession {
        id_store: conn.send_id_store.clone(),
        dst_map: Default::default(),
        unistream_map: Default::default(),
    };
    let quic_conn = conn.conn.clone();
    STATS.connection_opened();
    
    // Pre-allocate buffers for the hot path
    let mut datagram_buf = BytesMut::with_capacity(2100);
    let mut header_buf = Vec::with_capacity(64);
    
    loop {
        let (bytes, dst) = down_stream.recv_from().await?;
        STATS.packet_sent(bytes.len());
        
        let (id, is_new) = session.get_id_or_insert(&dst).await;
        
        // Send control header first if new
        if is_new {
            header_buf.clear();
            SQUdpControlHeader { dst: dst.clone(), id }.encode(&mut header_buf).await?;
            send.write_all(&header_buf).await?;
        }
        
        if over_stream {
            use std::collections::hash_map::Entry;
            let uni_conn = match session.unistream_map.entry(dst.clone()) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let (uni, _id) = conn.open_uni().await?;
                    e.insert(uni)
                }
            };
            header_buf.clear();
            if is_new {
                SQPacketDatagramHeader { id }.encode(&mut header_buf).await?;
            }
            (bytes.len() as u16).encode(&mut header_buf).await?;
            uni_conn.write_all(&header_buf).await?;
            uni_conn.write_all(&bytes).await?;
        } else {
            // Datagram path - zero-copy style
            header_buf.clear();
            SQPacketDatagramHeader { id }.encode(&mut header_buf).await?;
            datagram_buf.clear();
            datagram_buf.extend_from_slice(&header_buf);
            datagram_buf.extend_from_slice(&bytes);
            quic_conn.send_datagram(datagram_buf.split().freeze()).await?;
        }
    }
    #[allow(unreachable_code)]
    Ok(())
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
        id_map: Default::default(),
    };
    loop {
        let SQUdpControlHeader { id, dst } = SQUdpControlHeader::decode(&mut recv).await?;
        trace!("udp control header received: id:{},dst:{}", id, dst);
        session.store_socket(id, dst, udp_socket.clone()).await;
    }
    #[allow(unreachable_code)]
    Ok(())
}

/// Handle udp packet receive task
/// It watches udp packets from quic connection and sends them to the udp socket.
/// The udp socket could be downstream(inbound) or upstream(outbound)
/// This function is symetrical for both clients and servers.
pub async fn handle_udp_packet_recv<C: QuicConnection>(conn: SQConn<C>) -> Result<(), SError> {
    let id_store = conn.recv_id_store.clone();

    wait_sunny_auth(&conn).await?;
    STATS.connection_opened();
    loop {
        tokio::select! {
            b = conn.read_datagram() => {
                let b = b?;
                STATS.packet_received(b.len());
                
                // Zero-copy: decode u16 id directly (2 bytes, big endian)
                let id = u16::from_be_bytes([b[0], b[1]]);
                let payload = b.slice(2..);

                match id_store.get_socket_or_notify(id).await {
                 Ok((udp,addr)) =>  {
                    udp.send_to(payload, addr).await?;
                }
                Err(mut notify) =>  {
                    let id_store = id_store.clone();
                    let src_addr = conn.remote_address();
                    event!(Level::TRACE, "resolving datagram id:{}",id);
                    let payload = b;
                    tokio::spawn(async move {
                        let _ = notify.changed().await.map_err(|_|debug!("id:{} notifier dropped",id));
                        let (udp,addr) = id_store.try_get_socket(id).await.ok_or(SError::UDPSessionClosed(String::from("UDP session closed")))?;
                        info!("udp over datagram: id:{}: {}->{}",id, src_addr, addr);
                        let payload = payload.slice(2..);
                        let _ = udp.send_to(payload, addr).await
                        .map_err(|x|error!("{}",x));
                        Ok(()) as Result<(), SError>
                     }.in_current_span());
                }
            }
            }

            r = async {
                let (mut uni_stream, _id) = conn.accept_uni().await?;
                trace!("unistream accepted");
                let SQPacketDatagramHeader{id} = SQPacketDatagramHeader::decode(&mut uni_stream).await?;
                event!(Level::TRACE, "resolving datagram id:{}",id);

                let (udp,addr) = id_store.get_socket_or_wait(id).await?;

                info!("udp over stream: id:{}: {}->{}",id, conn.remote_address(), addr);
                Ok((uni_stream,udp.clone(),addr.clone())) as Result<(C::RecvStream,AnyUdpSend,SocksAddr),SError>
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

                tokio::spawn(async move {
                    loop {
                        let l: usize = u16::decode(&mut uni_stream).await? as usize;
                        let mut b = BytesMut::with_capacity(l);
                        b.resize(l,0);
                        uni_stream.read_exact(&mut b).await?;
                        udp.send_to(b.freeze(), addr.clone()).await?;
                    }
                    #[allow(unreachable_code)]
                    (Ok(()) as Result<(), SError>)
                }.in_current_span());
            }
        }
    }
    #[allow(unreachable_code)]
    Ok(())
}
