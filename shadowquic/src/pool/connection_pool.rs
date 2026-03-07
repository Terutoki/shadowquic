use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rustc_hash::FxHasher;

use crate::error::SError;
use crate::msgs::socks5::SocksAddr;
use crate::quic::{QuicConnection, QuicErrorRepr};

const DEFAULT_SHARD_COUNT: usize = 8;
const DEFAULT_SHARD_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct ShardedConnectionPool<C> {
    shards: Vec<Arc<ConnectionShard<C>>>,
    shard_mask: usize,
}

pub struct ConnectionShard<C> {
    connections: RwLock<HashMap<SocketAddr, Arc<ConnectionState<C>>>>,
    config: PoolConfig,
    stats: Arc<PoolStats>,
}

pub struct PoolConfig {
    pub max_connections_per_shard: usize,
    pub connection_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_idle_per_host: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_shard: DEFAULT_SHARD_CAPACITY,
            connection_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            max_idle_per_host: 2,
        }
    }
}

impl Clone for PoolConfig {
    fn clone(&self) -> Self {
        Self {
            max_connections_per_shard: self.max_connections_per_shard,
            connection_timeout: self.connection_timeout,
            idle_timeout: self.idle_timeout,
            max_idle_per_host: self.max_idle_per_host,
        }
    }
}

#[derive(Clone)]
pub struct PoolStats {
    pub total_connections: Arc<AtomicUsize>,
    pub active_connections: Arc<AtomicUsize>,
    pub connection_acquires: Arc<AtomicUsize>,
    pub connection_releases: Arc<AtomicUsize>,
    pub connection_errors: Arc<AtomicUsize>,
    pub connection_timeouts: Arc<AtomicUsize>,
}

impl Default for PoolStats {
    fn default() -> Self {
        Self {
            total_connections: Arc::new(AtomicUsize::new(0)),
            active_connections: Arc::new(AtomicUsize::new(0)),
            connection_acquires: Arc::new(AtomicUsize::new(0)),
            connection_releases: Arc::new(AtomicUsize::new(0)),
            connection_errors: Arc::new(AtomicUsize::new(0)),
            connection_timeouts: Arc::new(AtomicUsize::new(0)),
        }
    }
}

pub struct ConnectionState<C> {
    conn: RwLock<Option<C>>,
    in_use: AtomicBool,
    last_used: RwLock<Instant>,
    created_at: Instant,
    remote_addr: SocketAddr,
}

impl<C> ConnectionState<C> {
    fn new(conn: C, remote_addr: SocketAddr) -> Self {
        Self {
            conn: RwLock::new(Some(conn)),
            in_use: AtomicBool::new(false),
            last_used: RwLock::new(Instant::now()),
            created_at: Instant::now(),
            remote_addr,
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
        *self.last_used.write() = Instant::now();
        self.in_use.store(false, Ordering::Release);
    }

    pub fn is_expired(&self, config: &PoolConfig) -> bool {
        let idle = *self.last_used.read();
        let in_use = self.in_use.load(Ordering::Acquire);
        
        !in_use && idle.elapsed() > config.idle_timeout
    }

    pub fn take_connection(&self) -> Option<C> {
        self.conn.write().take()
    }

    pub fn connection(&self) -> Option<parking_lot::RwLockReadGuard<Option<C>>> {
        Some(self.conn.read())
    }
}

impl<C: QuicConnection> ShardedConnectionPool<C> {
    pub fn new(num_shards: usize, config: PoolConfig) -> Self {
        let num_shards = num_shards.next_power_of_two();
        let shards = (0..num_shards)
            .map(|_| {
                Arc::new(ConnectionShard {
                    connections: RwLock::new(HashMap::with_capacity(DEFAULT_SHARD_CAPACITY)),
                    config: config.clone(),
                    stats: Arc::new(PoolStats::default()),
                })
            })
            .collect();

        Self {
            shards,
            shard_mask: num_shards - 1,
        }
    }

    #[inline]
    fn get_shard(&self, addr: &SocketAddr) -> &Arc<ConnectionShard<C>> {
        let mut hasher = FxHasher::default();
        addr.hash(&mut hasher);
        &self.shards[(hasher.finish() as usize) & self.shard_mask]
    }

    pub async fn get<F, Fut>(&self, addr: SocketAddr, connect_fn: F) -> Result<Arc<ConnectionState<C>>, SError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<C, QuicErrorRepr>>,
    {
        let shard = self.get_shard(&addr);
        shard.get(addr, connect_fn).await
    }

    pub fn release(&self, state: &Arc<ConnectionState<C>>) {
        state.release();
    }

    pub fn stats(&self) -> PooledConnectionStats {
        let mut total = PooledConnectionStats::default();
        
        for shard in &self.shards {
            total.total += shard.stats.total_connections.load(Ordering::Relaxed);
            total.active += shard.stats.active_connections.load(Ordering::Relaxed);
            total.acquires += shard.stats.connection_acquires.load(Ordering::Relaxed);
            total.releases += shard.stats.connection_releases.load(Ordering::Relaxed);
            total.errors += shard.stats.connection_errors.load(Ordering::Relaxed);
            total.timeouts += shard.stats.connection_timeouts.load(Ordering::Relaxed);
        }
        
        total
    }
}

impl<C: QuicConnection> ConnectionShard<C> {
    pub async fn get<F, Fut>(&self, addr: SocketAddr, connect_fn: F) -> Result<Arc<ConnectionState<C>>, SError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<C, QuicErrorRepr>>,
    {
        {
            let connections = self.connections.read();
            if let Some(state) = connections.get(&addr) {
                if state.try_acquire() {
                    *state.last_used.write() = Instant::now();
                    self.stats.connection_acquires.fetch_add(1, Ordering::Relaxed);
                    return Ok(Arc::clone(state));
                }
            }
        }

        let mut connections = self.connections.write();
        
        if let Some(state) = connections.get(&addr) {
            if state.try_acquire() {
                *state.last_used.write() = Instant::now();
                return Ok(Arc::clone(state));
            }
        }

        if connections.len() >= self.config.max_connections_per_shard {
            self.cleanup_expired(&mut connections);
            if connections.len() >= self.config.max_connections_per_shard {
                return Err(SError::OutboundUnavailable);
            }
        }

        let conn = tokio::time::timeout(self.config.connection_timeout, connect_fn())
            .await
            .map_err(|_| {
                self.stats.connection_timeouts.fetch_add(1, Ordering::Relaxed);
                SError::OutboundUnavailable
            })?
            .map_err(|e| {
                self.stats.connection_errors.fetch_add(1, Ordering::Relaxed);
                SError::QuicError(QuicErrorRepr::QuicConnect(e.to_string()))
            })?;

        let state = Arc::new(ConnectionState::new(conn, addr));
        state.try_acquire();
        
        connections.insert(addr, Arc::clone(&state));
        
        self.stats.total_connections.fetch_add(1, Ordering::Relaxed);
        self.stats.active_connections.fetch_add(1, Ordering::Relaxed);
        self.stats.connection_acquires.fetch_add(1, Ordering::Relaxed);

        Ok(state)
    }

    fn cleanup_expired(&self, connections: &mut HashMap<SocketAddr, Arc<ConnectionState<C>>>) {
        connections.retain(|_, state| {
            if state.is_expired(&self.config) {
                let _ = state.take_connection();
                false
            } else {
                true
            }
        });
    }
}

#[derive(Debug, Default)]
pub struct PooledConnectionStats {
    pub total: usize,
    pub active: usize,
    pub acquires: usize,
    pub releases: usize,
    pub errors: usize,
    pub timeouts: usize,
}
