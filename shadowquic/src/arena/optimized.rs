use bytes::BytesMut;
use crossbeam::queue::SegQueue;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const DEFAULT_BUFFER_SIZE: usize = 2048;
const LARGE_BUFFER_SIZE: usize = 65535;

#[derive(Clone, Default)]
pub struct ArenaStats {
    pub allocated: Arc<AtomicUsize>,
    pub reused: Arc<AtomicUsize>,
    pub allocated_large: Arc<AtomicUsize>,
    pub reused_large: Arc<AtomicUsize>,
}

pub struct PacketArena {
    small: SegQueue<BytesMut>,
    large: SegQueue<BytesMut>,
    stats: ArenaStats,
}

impl PacketArena {
    pub fn new(small_count: usize, large_count: usize) -> Self {
        let small = SegQueue::new();
        let large = SegQueue::new();

        for _ in 0..small_count / 4 {
            let _ = small.push(BytesMut::with_capacity(DEFAULT_BUFFER_SIZE));
        }
        for _ in 0..large_count / 4 {
            let _ = large.push(BytesMut::with_capacity(LARGE_BUFFER_SIZE));
        }

        Self {
            small,
            large,
            stats: ArenaStats::default(),
        }
    }

    #[inline(always)]
    pub fn get(&self) -> BytesMut {
        self.small.pop().unwrap_or_else(|| {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(DEFAULT_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn get_large(&self) -> BytesMut {
        self.large.pop().unwrap_or_else(|| {
            self.stats.allocated_large.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(LARGE_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn put(&self, mut buf: BytesMut) {
        let cap = buf.capacity();
        buf.clear();

        if cap <= DEFAULT_BUFFER_SIZE {
            let _ = self.small.push(buf);
            self.stats.reused.fetch_add(1, Ordering::Relaxed);
        } else if cap <= LARGE_BUFFER_SIZE {
            let _ = self.large.push(buf);
            self.stats.reused_large.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.stats.allocated.load(Ordering::Relaxed),
            self.stats.reused.load(Ordering::Relaxed),
            self.stats.allocated_large.load(Ordering::Relaxed),
            self.stats.reused_large.load(Ordering::Relaxed),
        )
    }
}

pub static PACKET_ARENA: LazyLock<PacketArena> = LazyLock::new(|| PacketArena::new(4096, 256));

#[inline(always)]
pub fn packet_buf() -> BytesMut {
    PACKET_ARENA.get()
}

#[inline(always)]
pub fn packet_buf_large() -> BytesMut {
    PACKET_ARENA.get_large()
}

#[inline(always)]
pub fn packet_buf_sized(size: usize) -> BytesMut {
    if size <= DEFAULT_BUFFER_SIZE {
        PACKET_ARENA.get()
    } else if size <= LARGE_BUFFER_SIZE {
        PACKET_ARENA.get_large()
    } else {
        BytesMut::with_capacity(size)
    }
}

#[inline(always)]
pub fn packet_buf_put(buf: BytesMut) {
    PACKET_ARENA.put(buf)
}

pub type PacketArenaOptimized = PacketArena;
pub const PACKET_ARENA_OPTIMIZED: &LazyLock<PacketArena> = &PACKET_ARENA;
