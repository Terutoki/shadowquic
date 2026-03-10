use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use parking_lot::RwLock;
use rustc_hash::FxHasher;

use crate::msgs::socks5::SocksAddr;

const DEFAULT_SHARD_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub struct FastSessionData {
    pub id: u32,
    pub remote_addr: SocketAddr,
    pub bind_addr: SocksAddr,
    pub session_type: FastSessionType,
    pub created_at: u64,
    pub bytes_sent: u32,
    pub bytes_received: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FastSessionType {
    #[default]
    Tcp,
    UdpDatagram,
    UdpStream,
}

impl FastSessionData {
    #[inline]
    pub fn new(
        id: u32,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: FastSessionType,
    ) -> Self {
        Self {
            id,
            remote_addr,
            bind_addr,
            session_type,
            created_at: Instant::now().elapsed().as_secs(),
            bytes_sent: 0,
            bytes_received: 0,
        }
    }
}

pub struct FastSessionState {
    pub last_active: AtomicU64,
    pub in_use: AtomicBool,
    pub bytes_sent: AtomicUsize,
    pub bytes_received: AtomicUsize,
}

impl FastSessionState {
    pub fn new() -> Self {
        Self {
            last_active: AtomicU64::new(Instant::now().elapsed().as_secs()),
            in_use: AtomicBool::new(true),
            bytes_sent: AtomicUsize::new(0),
            bytes_received: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn is_available(&self) -> bool {
        !self.in_use.load(Ordering::Acquire)
    }

    #[inline]
    pub fn try_acquire(&self) -> bool {
        self.in_use
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    pub fn release(&self) {
        self.last_active
            .store(Instant::now().elapsed().as_secs(), Ordering::Release);
        self.in_use.store(false, Ordering::Release);
    }

    #[inline]
    pub fn last_active(&self) -> u64 {
        self.last_active.load(Ordering::Acquire)
    }
}

pub struct FastSessionManager {
    sessions: RwLock<HashMap<u32, (FastSessionData, FastSessionState)>>,
    id_gen: std::sync::atomic::AtomicU32,
    stats: FastSessionStats,
}

#[derive(Clone)]
pub struct FastSessionStats {
    pub total: Arc<AtomicUsize>,
    pub active: Arc<AtomicUsize>,
    pub tcp: Arc<AtomicUsize>,
    pub udp: Arc<AtomicUsize>,
    pub created: Arc<AtomicUsize>,
    pub closed: Arc<AtomicUsize>,
}

impl Default for FastSessionStats {
    fn default() -> Self {
        Self {
            total: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            tcp: Arc::new(AtomicUsize::new(0)),
            udp: Arc::new(AtomicUsize::new(0)),
            created: Arc::new(AtomicUsize::new(0)),
            closed: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl FastSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(DEFAULT_SHARD_CAPACITY)),
            id_gen: std::sync::atomic::AtomicU32::new(1),
            stats: FastSessionStats::default(),
        }
    }

    #[inline]
    pub fn create_session(
        &self,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: FastSessionType,
    ) -> FastSessionData {
        let id = self.id_gen.fetch_add(1, Ordering::Relaxed);
        let data = FastSessionData::new(id, remote_addr, bind_addr, session_type);
        let state = FastSessionState::new();

        self.sessions.write().insert(id, (data.clone(), state));
        self.stats.total.fetch_add(1, Ordering::Relaxed);
        self.stats.active.fetch_add(1, Ordering::Relaxed);
        self.stats.created.fetch_add(1, Ordering::Relaxed);

        match session_type {
            FastSessionType::Tcp => {
                (*self.stats.tcp).fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                (*self.stats.udp).fetch_add(1, Ordering::Relaxed);
            }
        }

        data
    }

    #[inline]
    pub fn get_session(&self, id: u32) -> Option<FastSessionData> {
        self.sessions.read().get(&id).map(|(d, _)| d.clone())
    }

    #[inline]
    pub fn try_get_session(&self, id: u32) -> Option<FastSessionData> {
        self.sessions.read().get(&id).map(|(d, _)| d.clone())
    }

    #[inline]
    pub fn release_session(&self, id: u32) {
        if let Some((_, state)) = self.sessions.read().get(&id) {
            state.release();
            self.stats.active.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn close_session(&self, id: u32) {
        if let Some((data, state)) = self.sessions.write().remove(&id) {
            state.release();
            self.stats.total.fetch_sub(1, Ordering::Relaxed);
            self.stats.closed.fetch_add(1, Ordering::Relaxed);

            match data.session_type {
                FastSessionType::Tcp => {
                    (*self.stats.tcp).fetch_sub(1, Ordering::Relaxed);
                }
                _ => {
                    (*self.stats.udp).fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn stats(&self) -> FastSessionStatsSnapshot {
        FastSessionStatsSnapshot {
            total: self.stats.total.load(Ordering::Relaxed),
            active: self.stats.active.load(Ordering::Relaxed),
            tcp: self.stats.tcp.load(Ordering::Relaxed),
            udp: self.stats.udp.load(Ordering::Relaxed),
            created: self.stats.created.load(Ordering::Relaxed),
            closed: self.stats.closed.load(Ordering::Relaxed),
        }
    }

    pub fn cleanup_expired(&self) {
        let now = Instant::now().elapsed().as_secs();
        let mut to_remove = Vec::new();

        {
            let sessions = self.sessions.read();
            for (id, (_, state)) in sessions.iter() {
                if !state.in_use.load(Ordering::Acquire) && (now - state.last_active()) > 60 {
                    to_remove.push(*id);
                }
            }
        }

        for id in to_remove {
            self.close_session(id);
        }
    }
}

impl Default for FastSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct FastSessionStatsSnapshot {
    pub total: usize,
    pub active: usize,
    pub tcp: usize,
    pub udp: usize,
    pub created: usize,
    pub closed: usize,
}

pub struct FastShardedSessionManager {
    shards: Vec<Arc<FastSessionManager>>,
    shard_mask: usize,
}

impl FastShardedSessionManager {
    pub fn new(num_shards: usize) -> Self {
        let num_shards = num_shards.next_power_of_two();
        let shards = (0..num_shards)
            .map(|_| Arc::new(FastSessionManager::new()))
            .collect();

        Self {
            shards,
            shard_mask: num_shards - 1,
        }
    }

    #[inline]
    fn get_shard(&self, addr: &SocketAddr) -> &Arc<FastSessionManager> {
        let mut hasher = FxHasher::default();
        addr.hash(&mut hasher);
        &self.shards[(hasher.finish() as usize) & self.shard_mask]
    }

    #[inline]
    fn get_shard_by_id(&self, id: u32) -> &Arc<FastSessionManager> {
        &self.shards[(id as usize) & self.shard_mask]
    }

    pub fn create_session(
        &self,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: FastSessionType,
    ) -> FastSessionData {
        let shard = self.get_shard(&remote_addr);
        shard.create_session(remote_addr, bind_addr, session_type)
    }

    pub fn get_session(&self, id: u32) -> Option<FastSessionData> {
        let shard = self.get_shard_by_id(id);
        shard.get_session(id)
    }

    pub fn try_get_session(&self, id: u32) -> Option<FastSessionData> {
        let shard = self.get_shard_by_id(id);
        shard.try_get_session(id)
    }

    pub fn release_session(&self, id: u32) {
        let shard = self.get_shard_by_id(id);
        shard.release_session(id)
    }

    pub fn close_session(&self, id: u32) {
        let shard = self.get_shard_by_id(id);
        shard.close_session(id)
    }

    pub fn cleanup_expired(&self) {
        for shard in &self.shards {
            shard.cleanup_expired();
        }
    }

    pub fn stats(&self) -> Vec<FastSessionStatsSnapshot> {
        self.shards.iter().map(|s| s.stats()).collect()
    }
}

impl Default for FastShardedSessionManager {
    fn default() -> Self {
        Self::new(16)
    }
}

use std::sync::LazyLock;

static FAST_SESSION_MANAGER: LazyLock<FastShardedSessionManager> =
    LazyLock::new(|| FastShardedSessionManager::new(16));

#[inline]
pub fn fast_create_session(
    remote_addr: SocketAddr,
    bind_addr: SocksAddr,
    session_type: FastSessionType,
) -> FastSessionData {
    FAST_SESSION_MANAGER.create_session(remote_addr, bind_addr, session_type)
}

#[inline]
pub fn fast_get_session(id: u32) -> Option<FastSessionData> {
    FAST_SESSION_MANAGER.get_session(id)
}

#[inline]
pub fn fast_release_session(id: u32) {
    FAST_SESSION_MANAGER.release_session(id)
}

#[inline]
pub fn fast_close_session(id: u32) {
    FAST_SESSION_MANAGER.close_session(id)
}
