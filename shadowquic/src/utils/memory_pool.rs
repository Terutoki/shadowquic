#![allow(dead_code)]

use bytes::BytesMut;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

const CACHE_LINE_SIZE: usize = 64;
const DEFAULT_POOL_SIZE: usize = 4096;
const SMALL_BUFFER_SIZE: usize = 256;
const MEDIUM_BUFFER_SIZE: usize = 2048;
const LARGE_BUFFER_SIZE: usize = 65536;

#[repr(align(64))]
struct CachePadded<T>(T);

pub struct HighPerfMemoryPool {
    small: CachePadded<AtomicUsize>,
    medium: CachePadded<AtomicUsize>,
    large: CachePadded<AtomicUsize>,
    stats: PoolStats,
}

pub struct PoolStats {
    pub allocated: AtomicUsize,
    pub freed: AtomicUsize,
    pub reused: AtomicUsize,
}

impl Default for PoolStats {
    fn default() -> Self {
        Self {
            allocated: AtomicUsize::new(0),
            freed: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
        }
    }
}

unsafe impl Send for HighPerfMemoryPool {}
unsafe impl Sync for HighPerfMemoryPool {}

impl HighPerfMemoryPool {
    pub fn new() -> Self {
        Self {
            small: CachePadded(AtomicUsize::new(0)),
            medium: CachePadded(AtomicUsize::new(0)),
            large: CachePadded(AtomicUsize::new(0)),
            stats: PoolStats::default(),
        }
    }

    #[inline]
    pub fn alloc(&self, size: usize) -> Vec<u8> {
        if size <= SMALL_BUFFER_SIZE {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            vec![0u8; size]
        } else if size <= MEDIUM_BUFFER_SIZE {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            vec![0u8; size]
        } else if size <= LARGE_BUFFER_SIZE {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            vec![0u8; size]
        } else {
            self.stats.allocated.fetch_add(1, Ordering::Relaxed);
            vec![0u8; size]
        }
    }

    #[inline]
    pub fn free(&self, _ptr: Vec<u8>) {
        self.stats.freed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let allocated = self.stats.allocated.load(Ordering::Relaxed);
        let freed = self.stats.freed.load(Ordering::Relaxed);
        let reused = self.stats.reused.load(Ordering::Relaxed);
        (allocated, freed, reused)
    }
}

impl Default for HighPerfMemoryPool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BytesPool {
    small_buffers: Vec<UnsafeCell<BytesMut>>,
    medium_buffers: Vec<UnsafeCell<BytesMut>>,
    large_buffers: Vec<UnsafeCell<BytesMut>>,
    small_head: AtomicUsize,
    medium_head: AtomicUsize,
    large_head: AtomicUsize,
}

unsafe impl Send for BytesPool {}
unsafe impl Sync for BytesPool {}

impl BytesPool {
    pub fn new() -> Self {
        let small_buffers: Vec<_> = (0..64)
            .map(|_| UnsafeCell::new(BytesMut::with_capacity(SMALL_BUFFER_SIZE)))
            .collect();
        let medium_buffers: Vec<_> = (0..32)
            .map(|_| UnsafeCell::new(BytesMut::with_capacity(MEDIUM_BUFFER_SIZE)))
            .collect();
        let large_buffers: Vec<_> = (0..16)
            .map(|_| UnsafeCell::new(BytesMut::with_capacity(LARGE_BUFFER_SIZE)))
            .collect();

        Self {
            small_buffers,
            medium_buffers,
            large_buffers,
            small_head: AtomicUsize::new(0),
            medium_head: AtomicUsize::new(0),
            large_head: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn get_small(&self) -> BytesMut {
        let index = self.small_head.fetch_add(1, Ordering::Relaxed) % 64;
        unsafe {
            let ptr = self.small_buffers[index].get();
            let buf = &mut *ptr;
            if buf.capacity() == 0 {
                *buf = BytesMut::with_capacity(SMALL_BUFFER_SIZE);
            }
            buf.clear();
            std::mem::take(buf)
        }
    }

    #[inline]
    pub fn get_medium(&self) -> BytesMut {
        let index = self.medium_head.fetch_add(1, Ordering::Relaxed) % 32;
        unsafe {
            let ptr = self.medium_buffers[index].get();
            let buf = &mut *ptr;
            if buf.capacity() == 0 {
                *buf = BytesMut::with_capacity(MEDIUM_BUFFER_SIZE);
            }
            buf.clear();
            std::mem::take(buf)
        }
    }

    #[inline]
    pub fn get_large(&self) -> BytesMut {
        let index = self.large_head.fetch_add(1, Ordering::Relaxed) % 16;
        unsafe {
            let ptr = self.large_buffers[index].get();
            let buf = &mut *ptr;
            if buf.capacity() == 0 {
                *buf = BytesMut::with_capacity(LARGE_BUFFER_SIZE);
            }
            buf.clear();
            std::mem::take(buf)
        }
    }

    #[inline]
    pub fn get_sized(&self, size: usize) -> BytesMut {
        if size <= SMALL_BUFFER_SIZE {
            self.get_small()
        } else if size <= MEDIUM_BUFFER_SIZE {
            self.get_medium()
        } else {
            BytesMut::with_capacity(size)
        }
    }
}

impl Default for BytesPool {
    fn default() -> Self {
        Self::new()
    }
}

use crossbeam::queue::SegQueue;
use std::sync::LazyLock;

static BYTES_POOL: LazyLock<BytesPool> = LazyLock::new(BytesPool::new);

#[inline]
pub fn fast_alloc(size: usize) -> BytesMut {
    BYTES_POOL.get_sized(size)
}

#[inline]
pub fn fast_alloc_small() -> BytesMut {
    BYTES_POOL.get_small()
}

#[inline]
pub fn fast_alloc_medium() -> BytesMut {
    BYTES_POOL.get_medium()
}

#[inline]
pub fn fast_alloc_large() -> BytesMut {
    BYTES_POOL.get_large()
}
