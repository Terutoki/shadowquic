use bytes::BytesMut;
use crossbeam::queue::SegQueue;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const DEFAULT_BUFFER_SIZE: usize = 2048;
const LARGE_BUFFER_SIZE: usize = 65535;
const WARM_UP_COUNT: usize = 256;

pub struct PacketArena {
    small_buffers: SegQueue<BytesMut>,
    large_buffers: SegQueue<BytesMut>,
    stats: ArenaStats,
}

#[derive(Clone, Default)]
pub struct ArenaStats {
    pub allocated: Arc<AtomicUsize>,
    pub reused: Arc<AtomicUsize>,
    pub allocated_large: Arc<AtomicUsize>,
    pub reused_large: Arc<AtomicUsize>,
}

impl Default for PacketArena {
    fn default() -> Self {
        Self::new(1024, 64)
    }
}

impl PacketArena {
    pub fn new(_small_count: usize, _large_count: usize) -> Self {
        let small = SegQueue::new();
        let large = SegQueue::new();

        for _ in 0..WARM_UP_COUNT {
            small.push(BytesMut::with_capacity(DEFAULT_BUFFER_SIZE));
        }
        for _ in 0..(WARM_UP_COUNT / 4) {
            large.push(BytesMut::with_capacity(LARGE_BUFFER_SIZE));
        }

        Self {
            small_buffers: small,
            large_buffers: large,
            stats: ArenaStats::default(),
        }
    }

    #[inline(always)]
    pub fn get(&self) -> BytesMut {
        self.small_buffers.pop().unwrap_or_else(|| {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(DEFAULT_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn get_large(&self) -> BytesMut {
        self.large_buffers.pop().unwrap_or_else(|| {
            self.stats.allocated_large.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(LARGE_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn get_sized(&self, size: usize) -> BytesMut {
        if size <= DEFAULT_BUFFER_SIZE {
            self.get()
        } else if size <= LARGE_BUFFER_SIZE {
            self.get_large()
        } else {
            BytesMut::with_capacity(size)
        }
    }

    #[inline(always)]
    pub fn put(&self, mut buf: BytesMut) {
        let cap = buf.capacity();
        buf.clear();

        if cap <= DEFAULT_BUFFER_SIZE {
            let _ = self.small_buffers.push(buf);
            self.stats.reused.fetch_add(1, Ordering::Relaxed);
        } else if cap <= LARGE_BUFFER_SIZE {
            let _ = self.large_buffers.push(buf);
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

pub static PACKET_ARENA: LazyLock<PacketArena> = LazyLock::new(|| PacketArena::new(1024, 64));

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
    PACKET_ARENA.get_sized(size)
}

#[inline(always)]
pub fn packet_buf_put(buf: BytesMut) {
    PACKET_ARENA.put(buf)
}

use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arena() {
        let arena = PacketArena::new(64, 16);

        let buf = arena.get();
        assert!(buf.capacity() >= DEFAULT_BUFFER_SIZE);

        arena.put(buf);

        let buf2 = arena.get();
        assert!(buf2.capacity() >= DEFAULT_BUFFER_SIZE);
    }
}
