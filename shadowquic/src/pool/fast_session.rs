use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
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
    pub created_at: u32,
    pub last_active: u32,
    pub in_use: bool,
    pub bytes_sent: usize,
    pub bytes_received: usize,
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
        let now = Instant::now().elapsed().as_secs() as u32;
        Self {
            id,
            remote_addr,
            bind_addr,
            session_type,
            created_at: now,
            last_active: now,
            in_use: true,
            bytes_sent: 0,
            bytes_received: 0,
        }
    }
}

pub struct FastSessionState {
    pub last_active: AtomicU32,
    pub in_use: AtomicBool,
    pub bytes_sent: AtomicUsize,
    pub bytes_received: AtomicUsize,
}

impl FastSessionState {
    pub fn new() -> Self {
        let now = Instant::now().elapsed().as_secs() as u32;
        Self {
            last_active: AtomicU32::new(now),
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
        let now = Instant::now().elapsed().as_secs() as u32;
        self.last_active.store(now, Ordering::Release);
        self.in_use.store(false, Ordering::Release);
    }
}

pub struct FastSessionManager {
    sessions: RwLock<HashMap<u32, (FastSessionData, FastSessionState)>>,
    id_gen: AtomicU32,
    stats: FastSessionStats,
}

pub struct FastSessionStats {
    pub total_sessions: AtomicUsize,
    pub active_sessions: AtomicUsize,
    pub session_creates: AtomicUsize,
    pub session_closes: AtomicUsize,
}

impl Default for FastSessionStats {
    fn default() -> Self {
        Self {
            total_sessions: AtomicUsize::new(0),
            active_sessions: AtomicUsize::new(0),
            session_creates: AtomicUsize::new(0),
            session_closes: AtomicUsize::new(0),
        }
    }
}

impl FastSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(DEFAULT_SHARD_CAPACITY)),
            id_gen: AtomicU32::new(1),
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
        self.stats.total_sessions.fetch_add(1, Ordering::Relaxed);
        self.stats.active_sessions.fetch_add(1, Ordering::Relaxed);
        self.stats.session_creates.fetch_add(1, Ordering::Relaxed);

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
            self.stats.active_sessions.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn close_session(&self, id: u32) {
        if let Some((_, state)) = self.sessions.write().remove(&id) {
            state.release();
            self.stats.total_sessions.fetch_sub(1, Ordering::Relaxed);
            self.stats.session_closes.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn stats(&self) -> FastSessionStatsSnapshot {
        FastSessionStatsSnapshot {
            total: self.stats.total_sessions.load(Ordering::Relaxed),
            active: self.stats.active_sessions.load(Ordering::Relaxed),
            created: self.stats.session_creates.load(Ordering::Relaxed),
            closed: self.stats.session_closes.load(Ordering::Relaxed),
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

    pub fn stats(&self) -> Vec<FastSessionStatsSnapshot> {
        self.shards.iter().map(|s| s.stats()).collect()
    }
}

impl Default for FastShardedSessionManager {
    fn default() -> Self {
        Self::new(8)
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
