use bytes::{BufMut, Bytes, BytesMut};
use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn get_numa_node_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/self/status") {
            for line in content.lines() {
                if line.starts_with("Node(s):") {
                    let parts = line.split_whitespace().skip(1).collect::<Vec<_>>();
                    if let Some(last) = parts.last() {
                        if let Some(max_node) = last.parse::<usize>().ok() {
                            return max_node + 1;
                        }
                    }
                }
            }
        }
    }
    1
}

pub fn get_current_numa_node() -> usize {
    0
}

pub fn bind_thread_to_numa_node(_node: usize) -> Result<(), std::io::Error> {
    Ok(())
}

pub struct NumaAwareAllocator {
    buffer_pool: PacketBufferPool,
    stats: NumaAllocatorStats,
}

#[derive(Clone, Default)]
pub struct NumaAllocatorStats {
    pub total_allocated: Arc<AtomicUsize>,
    pub total_freed: Arc<AtomicUsize>,
    pub cache_hits: Arc<AtomicUsize>,
    pub cache_misses: Arc<AtomicUsize>,
}

unsafe impl Send for NumaAwareAllocator {}
unsafe impl Sync for NumaAwareAllocator {}

impl NumaAwareAllocator {
    pub fn new(nodes: usize, slab_size: usize, slabs_per_node: usize) -> Self {
        let _ = (nodes, slab_size, slabs_per_node);

        Self {
            buffer_pool: PacketBufferPool::new(1),
            stats: NumaAllocatorStats::default(),
        }
    }

    #[inline]
    pub fn alloc(&self, size: usize) -> Option<BytesMut> {
        self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_allocated
            .fetch_add(size, Ordering::Relaxed);
        Some(BytesMut::with_capacity(size))
    }

    #[inline]
    pub fn alloc_exact(&self, size: usize) -> BytesMut {
        BytesMut::with_capacity(size)
    }

    pub fn stats(&self) -> NumaAllocatorStats {
        self.stats.clone()
    }

    pub fn node_count(&self) -> usize {
        1
    }

    pub fn set_node(&mut self, _node: usize) {}
}

pub struct PacketBufferPool {
    pools: Vec<UnsafeCell<Vec<BytesMut>>>,
    current_node: usize,
}

unsafe impl Send for PacketBufferPool {}
unsafe impl Sync for PacketBufferPool {}

impl PacketBufferPool {
    pub fn new(node_count: usize) -> Self {
        let node_count = if node_count > 0 { node_count } else { 1 };

        let mut pools = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            let mut pool = Vec::with_capacity(256);
            for _ in 0..64 {
                pool.push(BytesMut::with_capacity(4096));
                pool.push(BytesMut::with_capacity(16384));
                pool.push(BytesMut::with_capacity(65536));
            }
            pools.push(UnsafeCell::new(pool));
        }

        Self {
            pools,
            current_node: 0,
        }
    }

    #[inline]
    pub fn alloc(&self, size: usize) -> BytesMut {
        BytesMut::with_capacity(size)
    }

    #[inline]
    pub fn free(&self, mut buf: BytesMut) {
        buf.clear();
    }

    pub fn set_node(&mut self, node: usize) {
        self.current_node = node % self.pools.len();
    }
}
