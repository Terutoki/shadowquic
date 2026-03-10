pub mod buffer_pool;
pub mod connection_pool;
pub mod fast_session;
pub mod id_allocator;
pub mod optimized_session;
pub mod session_manager;

pub use buffer_pool::BufferPool;
pub use connection_pool::{PoolConfig, PoolStats, PooledConnectionStats, ShardedConnectionPool};
pub use fast_session::{
    FastSessionData, FastSessionManager, FastSessionStats, FastSessionStatsSnapshot,
    FastSessionType, FastShardedSessionManager,
};
pub use id_allocator::{IdAllocator, SessionId, SessionIdGenerator, ShardedIdAllocator};

pub use optimized_session::{
    OptimizedSessionData, OptimizedSessionManager, OptimizedSessionType as SessionType,
    OptimizedStatsSnapshot, PerCoreSessionManager, SessionData, SessionManager, SessionStats,
    SessionStatsSnapshot,
};

pub use fast_session::{
    fast_close_session, fast_create_session, fast_get_session, fast_release_session,
};
