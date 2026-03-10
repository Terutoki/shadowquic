pub mod batch_udp;
pub mod buffer;
pub mod dual_socket;
pub mod lock_free_ring;
pub mod memory_pool;
pub mod numa_allocator;
pub mod optimized_addr;
pub mod per_core_worker;
pub mod udp_batch;
pub mod batch_ops;

#[cfg(target_os = "android")]
pub mod protect_socket;

pub use batch_udp::{
    BatchUdpSocket, BatchUdpStats, BatchUdpWorker, DEFAULT_BATCH_SIZE, MAX_BATCH_SIZE,
    RecvMmsgBuffer, RecvPacket, SendMmsgBuffer, SendPacket,
};
pub use lock_free_ring::{
    BatchRingBuffer, LockFreeRingBuffer, MpscRingBuffer, RING_SIZE, RingEntry, RingStats,
    SpscRingBuffer,
};
pub use memory_pool::{BytesPool, HighPerfMemoryPool, PoolStats, fast_alloc, fast_alloc_small, fast_alloc_medium, fast_alloc_large};
pub use numa_allocator::{
    NumaAllocatorStats, NumaAwareAllocator, PacketBufferPool, bind_thread_to_numa_node,
    get_current_numa_node, get_numa_node_count,
};
pub use per_core_worker::{
    FlowDistributor, FlowKey, PerCoreWorker, PoolStats as WorkerPoolStats, WorkerConfig,
    WorkerLocal, WorkerPool, set_cpu_affinity,
};
pub use udp_batch::{
    OptimizedUdpSession, PacketQueue, QueueStats, SocketOptimizer, UdpBatchSocket,
};
pub use batch_ops::{PacketBatch, BatchEntry, BatchProcessor, AtomicBatch, BATCH_SIZE};
