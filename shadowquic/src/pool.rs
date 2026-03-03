use bytes::BytesMut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const POOL_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;

pub struct BufferPool {
    pool: Arc<Vec<AtomicUsize>>,
    total_allocated: Arc<AtomicUsize>,
    total_reused: Arc<AtomicUsize>,
}

impl BufferPool {
    pub fn new() -> Self {
        let pool = Arc::new((0..POOL_SIZE).map(|_| AtomicUsize::new(0)).collect());
        Self {
            pool,
            total_allocated: Arc::new(AtomicUsize::new(0)),
            total_reused: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[inline]
    pub fn alloc(&self) -> BytesMut {
        for (_i, slot) in self.pool.iter().enumerate() {
            let val = slot.load(Ordering::Acquire);
            if val > 0
                && slot
                    .compare_exchange(val, val - 1, Ordering::Release, Ordering::Acquire)
                    .is_ok()
            {
                self.total_reused.fetch_add(1, Ordering::Relaxed);
                return BytesMut::with_capacity(BUFFER_SIZE);
            }
        }
        self.total_allocated.fetch_add(1, Ordering::Relaxed);
        BytesMut::with_capacity(BUFFER_SIZE)
    }

    #[inline]
    pub fn free(&self, buf: &mut BytesMut) {
        if buf.capacity() == BUFFER_SIZE {
            for slot in self.pool.iter() {
                let val = slot.load(Ordering::Acquire);
                if val < POOL_SIZE
                    && slot
                        .compare_exchange(val, val + 1, Ordering::Release, Ordering::Acquire)
                        .is_ok()
                {
                    buf.clear();
                    return;
                }
            }
        }
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let allocated = self.total_allocated.load(Ordering::Relaxed);
        let reused = self.total_reused.load(Ordering::Relaxed);
        let total = allocated + reused;
        (total, allocated, reused)
    }
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BufferPool {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            total_allocated: Arc::clone(&self.total_allocated),
            total_reused: Arc::clone(&self.total_reused),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool() {
        let pool = BufferPool::new();
        let mut buf1 = pool.alloc();
        buf1.extend_from_slice(b"test");

        let (total, allocated, reused) = pool.stats();
        assert_eq!(total, 1);
        assert_eq!(allocated, 1);
        assert_eq!(reused, 0);

        pool.free(&mut buf1);

        let mut buf2 = pool.alloc();
        let (total, allocated, reused) = pool.stats();
        assert_eq!(total, 2);
        assert_eq!(allocated, 1);
        assert_eq!(reused, 1);
        assert!(buf2.is_empty());
    }
}
