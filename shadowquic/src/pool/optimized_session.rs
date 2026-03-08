use crate::msgs::socks5::SocksAddr;
use crate::pool::id_allocator::SessionIdGenerator;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

const SESSION_EXPIRE_SECS: u32 = 60;
const SHARD_COUNT: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizedSessionType {
    Tcp,
    UdpDatagram,
    UdpStream,
}

impl Default for OptimizedSessionType {
    fn default() -> Self {
        OptimizedSessionType::Tcp
    }
}

struct OptimizedSessionDataInner {
    id: u32,
    remote_addr: SocketAddr,
    bind_addr: SocksAddr,
    session_type: OptimizedSessionType,
    created_at: u64,
    last_active: std::sync::atomic::AtomicU64,
    in_use: AtomicBool,
    bytes_sent: AtomicUsize,
    bytes_received: AtomicUsize,
}

pub struct OptimizedSessionData {
    inner: OptimizedSessionDataInner,
}

impl OptimizedSessionData {
    #[inline]
    pub fn new(
        id: u32,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: OptimizedSessionType,
    ) -> Self {
        let now = Instant::now();
        Self {
            inner: OptimizedSessionDataInner {
                id,
                remote_addr,
                bind_addr,
                session_type,
                created_at: now.elapsed().as_secs(),
                last_active: std::sync::atomic::AtomicU64::new(now.elapsed().as_secs()),
                in_use: AtomicBool::new(true),
                bytes_sent: AtomicUsize::new(0),
                bytes_received: AtomicUsize::new(0),
            },
        }
    }

    #[inline]
    pub fn id(&self) -> u32 {
        self.inner.id
    }

    #[inline]
    pub fn remote_addr(&self) -> SocketAddr {
        self.inner.remote_addr
    }

    #[inline]
    pub fn bind_addr(&self) -> &SocksAddr {
        &self.inner.bind_addr
    }

    #[inline]
    pub fn session_type(&self) -> OptimizedSessionType {
        self.inner.session_type
    }

    #[inline]
    pub fn is_in_use(&self) -> bool {
        self.inner.in_use.load(Ordering::Acquire)
    }

    #[inline]
    pub fn mark_used(&self) {
        self.inner.in_use.store(true, Ordering::Release);
    }

    #[inline]
    pub fn mark_idle(&self) {
        self.inner.in_use.store(false, Ordering::Release);
    }

    #[inline]
    pub fn update_activity(&self) {
        self.inner
            .last_active
            .store(Instant::now().elapsed().as_secs(), Ordering::Release);
    }

    #[inline]
    pub fn is_expired(&self) -> bool {
        !self.inner.in_use.load(Ordering::Acquire)
            && (Instant::now().elapsed().as_secs() - self.inner.last_active.load(Ordering::Acquire))
                > SESSION_EXPIRE_SECS as u64
    }
}

impl Clone for OptimizedSessionData {
    fn clone(&self) -> Self {
        Self {
            inner: OptimizedSessionDataInner {
                id: self.inner.id,
                remote_addr: self.inner.remote_addr,
                bind_addr: self.inner.bind_addr.clone(),
                session_type: self.inner.session_type,
                created_at: self.inner.created_at,
                last_active: std::sync::atomic::AtomicU64::new(
                    self.inner.last_active.load(Ordering::Acquire),
                ),
                in_use: AtomicBool::new(self.inner.in_use.load(Ordering::Acquire)),
                bytes_sent: AtomicUsize::new(self.inner.bytes_sent.load(Ordering::Acquire)),
                bytes_received: AtomicUsize::new(self.inner.bytes_received.load(Ordering::Acquire)),
            },
        }
    }
}

struct OptimizedSessionShard {
    sessions: RwLock<FxHashMap<u32, OptimizedSessionData>>,
    id_gen: SessionIdGenerator,
    stats: OptimizedShardStats,
}

struct OptimizedShardStats {
    active: AtomicUsize,
    total: AtomicUsize,
    created: AtomicUsize,
    closed: AtomicUsize,
    tcp: AtomicUsize,
    udp: AtomicUsize,
}

impl Default for OptimizedShardStats {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            created: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            tcp: AtomicUsize::new(0),
            udp: AtomicUsize::new(0),
        }
    }
}

impl OptimizedSessionShard {
    fn new() -> Self {
        Self {
            sessions: RwLock::new(FxHashMap::with_capacity_and_hasher(
                2048,
                Default::default(),
            )),
            id_gen: SessionIdGenerator::new(),
            stats: OptimizedShardStats::default(),
        }
    }

    #[inline]
    pub fn create_session(
        &self,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: OptimizedSessionType,
    ) -> u32 {
        let id = self.id_gen.next();
        let session = OptimizedSessionData::new(id, remote_addr, bind_addr, session_type);

        self.sessions.write().insert(id, session);

        self.stats.created.fetch_add(1, Ordering::Relaxed);
        self.stats.active.fetch_add(1, Ordering::Relaxed);
        self.stats.total.fetch_add(1, Ordering::Relaxed);

        match session_type {
            OptimizedSessionType::Tcp => {
                self.stats.tcp.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.stats.udp.fetch_add(1, Ordering::Relaxed);
            }
        }

        id
    }

    #[inline]
    pub fn get_session(&self, id: u32) -> Option<OptimizedSessionData> {
        self.sessions.read().get(&id).cloned()
    }

    #[inline]
    pub fn release_session(&self, id: u32) {
        if let Some(session) = self.sessions.read().get(&id) {
            session.update_activity();
            session.mark_idle();
            self.stats.active.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn close_session(&self, id: u32) {
        if let Some(_session) = self.sessions.write().remove(&id) {
            self.stats.closed.fetch_add(1, Ordering::Relaxed);
            self.stats.total.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn cleanup_expired(&self) {
        let now = Instant::now().elapsed().as_secs();
        let mut to_remove = Vec::new();

        {
            let sessions = self.sessions.read();
            for (id, session) in sessions.iter() {
                let last_active = session.inner.last_active.load(Ordering::Acquire);
                let in_use = session.inner.in_use.load(Ordering::Acquire);
                if !in_use && (now - last_active) > SESSION_EXPIRE_SECS as u64 {
                    to_remove.push(*id);
                }
            }
        }

        for id in to_remove {
            if let Some(session) = self.sessions.write().remove(&id) {
                match session.session_type() {
                    OptimizedSessionType::Tcp => {
                        self.stats.tcp.fetch_sub(1, Ordering::Relaxed);
                    }
                    _ => {
                        self.stats.udp.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                self.stats.closed.fetch_add(1, Ordering::Relaxed);
                self.stats.total.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

pub struct PerCoreSessionManager {
    shards: Vec<OptimizedSessionShard>,
    shard_mask: usize,
}

impl PerCoreSessionManager {
    pub fn new() -> Self {
        let shard_count = SHARD_COUNT;
        let shards = (0..shard_count)
            .map(|_| OptimizedSessionShard::new())
            .collect();

        Self {
            shards,
            shard_mask: shard_count - 1,
        }
    }

    #[inline]
    fn get_shard(&self, id: u32) -> &OptimizedSessionShard {
        &self.shards[(id as usize) & self.shard_mask]
    }

    #[inline]
    fn get_shard_by_addr(&self, addr: &SocketAddr) -> &OptimizedSessionShard {
        let mut hasher = FxHasher::default();
        addr.hash(&mut hasher);
        &self.shards[(hasher.finish() as usize) & self.shard_mask]
    }

    #[inline]
    pub fn create_session(
        &self,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: OptimizedSessionType,
    ) -> u32 {
        self.get_shard_by_addr(&remote_addr)
            .create_session(remote_addr, bind_addr, session_type)
    }

    #[inline]
    pub fn get_session(&self, id: u32) -> Option<OptimizedSessionData> {
        self.get_shard(id).get_session(id)
    }

    #[inline]
    pub fn release_session(&self, id: u32) {
        self.get_shard(id).release_session(id)
    }

    #[inline]
    pub fn close_session(&self, id: u32) {
        self.get_shard(id).close_session(id)
    }

    pub fn cleanup_expired(&self) {
        for shard in &self.shards {
            shard.cleanup_expired();
        }
    }

    pub fn stats(&self) -> OptimizedStatsSnapshot {
        let mut total = 0;
        let mut active = 0;
        let mut created = 0;
        let mut closed = 0;
        let mut tcp = 0;
        let mut udp = 0;

        for shard in &self.shards {
            total += shard.stats.total.load(Ordering::Relaxed);
            active += shard.stats.active.load(Ordering::Relaxed);
            created += shard.stats.created.load(Ordering::Relaxed);
            closed += shard.stats.closed.load(Ordering::Relaxed);
            tcp += shard.stats.tcp.load(Ordering::Relaxed);
            udp += shard.stats.udp.load(Ordering::Relaxed);
        }

        OptimizedStatsSnapshot {
            total,
            active,
            created,
            closed,
            tcp,
            udp,
        }
    }
}

impl Default for PerCoreSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct OptimizedStatsSnapshot {
    pub total: usize,
    pub active: usize,
    pub created: usize,
    pub closed: usize,
    pub tcp: usize,
    pub udp: usize,
}

pub type OptimizedSessionManager = PerCoreSessionManager;

pub type SessionManager = PerCoreSessionManager;
pub type SessionStats = OptimizedStatsSnapshot;
pub type SessionStatsSnapshot = OptimizedStatsSnapshot;
pub type SessionType = OptimizedSessionType;

#[derive(Clone)]
pub struct SessionData {
    pub id: u32,
    pub remote_addr: std::net::SocketAddr,
    pub bind_addr: crate::msgs::socks5::SocksAddr,
    pub session_type: OptimizedSessionType,
}

impl From<OptimizedSessionData> for SessionData {
    fn from(opt: OptimizedSessionData) -> Self {
        SessionData {
            id: opt.inner.id,
            remote_addr: opt.inner.remote_addr,
            bind_addr: opt.inner.bind_addr,
            session_type: opt.inner.session_type,
        }
    }
}
