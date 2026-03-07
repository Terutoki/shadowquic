use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use parking_lot::RwLock;
use rustc_hash::FxHashMap;

use crate::msgs::socks5::SocksAddr;
use crate::pool::BufferPool;
use crate::pool::id_allocator::SessionIdGenerator;

const DEFAULT_SESSION_CAPACITY: usize = 16384;
const SESSION_EXPIRE_SECS: u32 = 60;

#[derive(Clone)]
pub struct SessionData {
    pub id: u32,
    pub remote_addr: SocketAddr,
    pub bind_addr: SocksAddr,
    pub session_type: SessionType,
    pub created_at: Instant,
    pub last_active: u64,
    pub in_use: bool,
    pub bytes_sent: u32,
    pub bytes_received: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionType {
    #[default]
    Tcp,
    UdpDatagram,
    UdpStream,
}

pub struct SessionManager {
    sessions: RwLock<FxHashMap<u32, SessionData>>,
    id_gen: SessionIdGenerator,
    buffer_pool: BufferPool,
    stats: SessionStats,
}

#[derive(Clone, Default)]
pub struct SessionStats {
    pub total_sessions: Arc<AtomicUsize>,
    pub active_sessions: Arc<AtomicUsize>,
    pub tcp_sessions: Arc<AtomicUsize>,
    pub udp_sessions: Arc<AtomicUsize>,
    pub sessions_created: Arc<AtomicUsize>,
    pub sessions_closed: Arc<AtomicUsize>,
    pub bytes_sent: Arc<AtomicU32>,
    pub bytes_received: Arc<AtomicU32>,
}

impl SessionManager {
    pub fn new(buffer_pool: BufferPool) -> Self {
        Self {
            sessions: RwLock::new(FxHashMap::with_capacity_and_hasher(
                DEFAULT_SESSION_CAPACITY,
                Default::default(),
            )),
            id_gen: SessionIdGenerator::new(),
            buffer_pool,
            stats: SessionStats::default(),
        }
    }

    #[inline]
    pub fn create_session(
        &self,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: SessionType,
    ) -> u32 {
        let id = self.id_gen.next();
        let now = Instant::now();
        let now_u64 = now.elapsed().as_secs();

        let session = SessionData {
            id,
            remote_addr,
            bind_addr,
            session_type,
            created_at: now,
            last_active: now_u64,
            in_use: true,
            bytes_sent: 0,
            bytes_received: 0,
        };

        self.sessions.write().insert(id, session);

        self.stats.sessions_created.fetch_add(1, Ordering::Relaxed);
        self.stats.total_sessions.fetch_add(1, Ordering::Relaxed);
        self.stats.active_sessions.fetch_add(1, Ordering::Relaxed);

        match session_type {
            SessionType::Tcp => {
                self.stats.tcp_sessions.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.stats.udp_sessions.fetch_add(1, Ordering::Relaxed);
            }
        }

        id
    }

    #[inline]
    pub fn get_session(&self, id: u32) -> Option<SessionData> {
        self.sessions.read().get(&id).cloned()
    }

    pub fn release_session(&self, id: u32) {
        let now_u64 = Instant::now().elapsed().as_secs();
        if let Some(session) = self.sessions.write().get_mut(&id) {
            session.in_use = false;
            session.last_active = now_u64;
        }
        self.stats.active_sessions.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn close_session(&self, id: u32) {
        if let Some(session) = self.sessions.write().remove(&id) {
            self.stats.sessions_closed.fetch_add(1, Ordering::Relaxed);
            self.stats.total_sessions.fetch_sub(1, Ordering::Relaxed);

            match session.session_type {
                SessionType::Tcp => {
                    self.stats.tcp_sessions.fetch_sub(1, Ordering::Relaxed);
                }
                _ => {
                    self.stats.udp_sessions.fetch_sub(1, Ordering::Relaxed);
                }
            }

            self.stats
                .bytes_sent
                .fetch_add(session.bytes_sent, Ordering::Relaxed);
            self.stats
                .bytes_received
                .fetch_add(session.bytes_received, Ordering::Relaxed);
        }
    }

    pub fn cleanup_expired(&self) {
        let now = Instant::now().elapsed().as_secs();
        let mut to_remove = Vec::new();

        {
            let sessions = self.sessions.read();
            for (id, session) in sessions.iter() {
                if !session.in_use && (now - session.last_active) > SESSION_EXPIRE_SECS as u64 {
                    to_remove.push(*id);
                }
            }
        }

        for id in to_remove {
            self.close_session(id);
        }
    }

    pub fn stats(&self) -> SessionStatsSnapshot {
        SessionStatsSnapshot {
            total: self.stats.total_sessions.load(Ordering::Relaxed),
            active: self.stats.active_sessions.load(Ordering::Relaxed),
            tcp: self.stats.tcp_sessions.load(Ordering::Relaxed),
            udp: self.stats.udp_sessions.load(Ordering::Relaxed),
            created: self.stats.sessions_created.load(Ordering::Relaxed),
            closed: self.stats.sessions_closed.load(Ordering::Relaxed),
            bytes_sent: self.stats.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.stats.bytes_received.load(Ordering::Relaxed),
        }
    }

    pub fn buffer_pool(&self) -> &BufferPool {
        &self.buffer_pool
    }
}

#[derive(Debug, Default)]
pub struct SessionStatsSnapshot {
    pub total: usize,
    pub active: usize,
    pub tcp: usize,
    pub udp: usize,
    pub created: usize,
    pub closed: usize,
    pub bytes_sent: u32,
    pub bytes_received: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgs::socks5::SocksAddr;

    #[test]
    fn test_session_creation() {
        let buffer_pool = BufferPool::new(64);
        let manager = SessionManager::new(buffer_pool);

        let remote: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let bind = SocksAddr::from_domain("example.com".to_string(), 443);

        let id = manager.create_session(remote, bind, SessionType::Tcp);

        assert!(manager.get_session(id).is_some());

        manager.release_session(id);

        let stats = manager.stats();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.active, 0);
    }
}
