use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use parking_lot::RwLock;

use crate::msgs::socks5::SocksAddr;
use crate::pool::BufferPool;
use crate::pool::id_allocator::SessionIdGenerator;

const DEFAULT_SESSION_CAPACITY: usize = 16384;
const SESSION_EXPIRE_SECS: u32 = 60;

pub struct SessionData {
    pub id: u32,
    pub remote_addr: SocketAddr,
    pub bind_addr: SocksAddr,
    pub session_type: SessionType,
    pub created_at: Instant,
    pub last_active: RwLock<Instant>,
    pub in_use: AtomicBool,
    pub bytes_sent: AtomicU32,
    pub bytes_received: AtomicU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionType {
    Tcp,
    UdpDatagram,
    UdpStream,
}

pub struct SessionManager {
    sessions: RwLock<HashMap<u32, SessionData>>,
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
            sessions: RwLock::new(HashMap::with_capacity(DEFAULT_SESSION_CAPACITY)),
            id_gen: SessionIdGenerator::new(),
            buffer_pool,
            stats: SessionStats::default(),
        }
    }

    pub fn create_session(
        &self,
        remote_addr: SocketAddr,
        bind_addr: SocksAddr,
        session_type: SessionType,
    ) -> u32 {
        let id = self.id_gen.next();

        let session = SessionData {
            id,
            remote_addr,
            bind_addr,
            session_type,
            created_at: Instant::now(),
            last_active: RwLock::new(Instant::now()),
            in_use: AtomicBool::new(true),
            bytes_sent: AtomicU32::new(0),
            bytes_received: AtomicU32::new(0),
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
    pub fn get_session(&self, id: u32) -> Option<Arc<SessionData>> {
        self.sessions.read().get(&id).map(|s| {
            Arc::new(SessionData {
                id: s.id,
                remote_addr: s.remote_addr,
                bind_addr: s.bind_addr.clone(),
                session_type: s.session_type,
                created_at: s.created_at,
                last_active: RwLock::new(*s.last_active.read()),
                in_use: AtomicBool::new(s.in_use.load(Ordering::Relaxed)),
                bytes_sent: AtomicU32::new(s.bytes_sent.load(Ordering::Relaxed)),
                bytes_received: AtomicU32::new(s.bytes_received.load(Ordering::Relaxed)),
            })
        })
    }

    pub fn release_session(&self, id: u32) {
        if let Some(session) = self.sessions.read().get(&id) {
            *session.last_active.write() = Instant::now();
            session.in_use.store(false, Ordering::Release);
            self.stats.active_sessions.fetch_sub(1, Ordering::Relaxed);
        }
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

            let bytes_sent = session.bytes_sent.load(Ordering::Relaxed);
            let bytes_received = session.bytes_received.load(Ordering::Relaxed);
            self.stats
                .bytes_sent
                .fetch_add(bytes_sent, Ordering::Relaxed);
            self.stats
                .bytes_received
                .fetch_add(bytes_received, Ordering::Relaxed);
        }
    }

    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        let mut to_remove = Vec::new();

        {
            let sessions = self.sessions.read();
            for (id, session) in sessions.iter() {
                if !session.in_use.load(Ordering::Acquire)
                    && now.duration_since(*session.last_active.read()).as_secs() as u32
                        > SESSION_EXPIRE_SECS
                {
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
