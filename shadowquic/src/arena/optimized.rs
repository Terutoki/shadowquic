use bytes::BytesMut;
use crossbeam::queue::SegQueue;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;

const DEFAULT_BUFFER_SIZE: usize = 2048;
const LARGE_BUFFER_SIZE: usize = 65535;
const HUGE_BUFFER_SIZE: usize = 1024 * 1024;

const TINY_BUFFER_SIZE: usize = 256;
const SMALL_BUFFER_SIZE: usize = 512;
const MEDIUM_BUFFER_SIZE: usize = 4096;

const TINY_POOL_SIZE: usize = 8192;
const SMALL_POOL_SIZE: usize = 4096;
const MEDIUM_POOL_SIZE: usize = 1024;
const LARGE_POOL_SIZE: usize = 256;

#[derive(Clone, Default)]
pub struct ArenaStats {
    pub allocated: Arc<AtomicUsize>,
    pub reused: Arc<AtomicUsize>,
    pub allocated_large: Arc<AtomicUsize>,
    pub reused_large: Arc<AtomicUsize>,
    pub huge_allocated: Arc<AtomicUsize>,
    pub huge_reused: Arc<AtomicUsize>,
}

pub struct PacketArena {
    tiny: SegQueue<BytesMut>,
    small: SegQueue<BytesMut>,
    medium: SegQueue<BytesMut>,
    large: SegQueue<BytesMut>,
    huge: SegQueue<BytesMut>,
    stats: ArenaStats,
}

impl PacketArena {
    pub fn new(
        tiny_count: usize,
        small_count: usize,
        medium_count: usize,
        large_count: usize,
        huge_count: usize,
    ) -> Self {
        let tiny = SegQueue::new();
        let small = SegQueue::new();
        let medium = SegQueue::new();
        let large = SegQueue::new();
        let huge = SegQueue::new();

        for _ in 0..tiny_count / 4 {
            let _ = tiny.push(BytesMut::with_capacity(TINY_BUFFER_SIZE));
        }
        for _ in 0..small_count / 4 {
            let _ = small.push(BytesMut::with_capacity(SMALL_BUFFER_SIZE));
        }
        for _ in 0..medium_count / 4 {
            let _ = medium.push(BytesMut::with_capacity(MEDIUM_BUFFER_SIZE));
        }
        for _ in 0..large_count / 4 {
            let _ = large.push(BytesMut::with_capacity(LARGE_BUFFER_SIZE));
        }
        for _ in 0..huge_count / 4 {
            let _ = huge.push(BytesMut::with_capacity(HUGE_BUFFER_SIZE));
        }

        Self {
            tiny,
            small,
            medium,
            large,
            huge,
            stats: ArenaStats::default(),
        }
    }

    #[inline(always)]
    pub fn get(&self) -> BytesMut {
        self.tiny.pop().unwrap_or_else(|| {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(TINY_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn get_sized(&self, size: usize) -> BytesMut {
        if size <= TINY_BUFFER_SIZE {
            self.get()
        } else if size <= SMALL_BUFFER_SIZE {
            self.get_small()
        } else if size <= MEDIUM_BUFFER_SIZE {
            self.get_medium()
        } else if size <= LARGE_BUFFER_SIZE {
            self.get_large()
        } else {
            self.get_huge()
        }
    }

    #[inline(always)]
    pub fn get_small(&self) -> BytesMut {
        self.small.pop().unwrap_or_else(|| {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(SMALL_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn get_medium(&self) -> BytesMut {
        self.medium.pop().unwrap_or_else(|| {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(MEDIUM_BUFFER_SIZE)
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
    pub fn get_huge(&self) -> BytesMut {
        self.huge.pop().unwrap_or_else(|| {
            self.stats.huge_allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(HUGE_BUFFER_SIZE)
        })
    }

    #[inline(always)]
    pub fn put(&self, mut buf: BytesMut) {
        let cap = buf.capacity();
        buf.clear();

        if cap <= TINY_BUFFER_SIZE {
            let _ = self.tiny.push(buf);
            self.stats.reused.fetch_add(1, Ordering::Relaxed);
        } else if cap <= SMALL_BUFFER_SIZE {
            let _ = self.small.push(buf);
            self.stats.reused.fetch_add(1, Ordering::Relaxed);
        } else if cap <= MEDIUM_BUFFER_SIZE {
            let _ = self.medium.push(buf);
            self.stats.reused.fetch_add(1, Ordering::Relaxed);
        } else if cap <= LARGE_BUFFER_SIZE {
            let _ = self.large.push(buf);
            self.stats.reused_large.fetch_add(1, Ordering::Relaxed);
        } else if cap <= HUGE_BUFFER_SIZE {
            let _ = self.huge.push(buf);
            self.stats.huge_reused.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn stats(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.stats.allocated.load(Ordering::Relaxed),
            self.stats.reused.load(Ordering::Relaxed),
            self.stats.allocated_large.load(Ordering::Relaxed),
            self.stats.reused_large.load(Ordering::Relaxed),
            self.stats.huge_allocated.load(Ordering::Relaxed),
            self.stats.huge_reused.load(Ordering::Relaxed),
        )
    }
}

pub static PACKET_ARENA: LazyLock<PacketArena> = LazyLock::new(|| {
    PacketArena::new(
        TINY_POOL_SIZE,
        SMALL_POOL_SIZE,
        MEDIUM_POOL_SIZE,
        LARGE_POOL_SIZE,
        64,
    )
});

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

pub type PacketArenaOptimized = PacketArena;
pub const PACKET_ARENA_OPTIMIZED: &LazyLock<PacketArena> = &PACKET_ARENA;

pub struct HugePageAllocator {
    large_buffer: SegQueue<BytesMut>,
    page_size: usize,
}

impl HugePageAllocator {
    pub fn new(page_size: usize) -> Self {
        let large_buffer = SegQueue::new();
        for _ in 0..64 {
            let _ = large_buffer.push(BytesMut::with_capacity(page_size * 2));
        }
        Self {
            large_buffer,
            page_size,
        }
    }

    #[inline]
    pub fn alloc(&self) -> BytesMut {
        self.large_buffer
            .pop()
            .unwrap_or_else(|| BytesMut::with_capacity(self.page_size * 2))
    }

    #[inline]
    pub fn free(&self, mut buf: BytesMut) {
        buf.clear();
        if buf.capacity() >= self.page_size {
            let _ = self.large_buffer.push(buf);
        }
    }
}

pub static HUGE_PAGE_ALLOCATOR: LazyLock<HugePageAllocator> = LazyLock::new(|| {
    #[cfg(target_os = "linux")]
    {
        let page_size: usize = 2 * 1024 * 1024; // Default 2MB
        HugePageAllocator::new(page_size)
    }
    #[cfg(not(target_os = "linux"))]
    {
        HugePageAllocator::new(2 * 1024 * 1024)
    }
});

#[inline(always)]
pub fn huge_page_buf() -> BytesMut {
    HUGE_PAGE_ALLOCATOR.alloc()
}

#[inline(always)]
pub fn huge_page_buf_put(buf: BytesMut) {
    HUGE_PAGE_ALLOCATOR.free(buf)
}
