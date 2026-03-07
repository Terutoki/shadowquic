use bytes::BytesMut;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const DEFAULT_POOL_SIZE: usize = 1024;
const BUFFER_SIZE: usize = 2048;

pub struct BufferPool {
    queue: crossbeam::queue::SegQueue<BytesMut>,
    capacity: usize,
    total_allocated: Arc<AtomicUsize>,
    total_reused: Arc<AtomicUsize>,
}

impl BufferPool {
    pub fn new(capacity: usize) -> Self {
        let queue = crossbeam::queue::SegQueue::new();

        let pool = Self {
            queue,
            capacity,
            total_allocated: Arc::new(AtomicUsize::new(0)),
            total_reused: Arc::new(AtomicUsize::new(0)),
        };

        pool.warm_up();
        pool
    }

    fn warm_up(&self) {
        for _ in 0..self.capacity / 4 {
            self.queue.push(BytesMut::with_capacity(BUFFER_SIZE));
        }
    }

    #[inline]
    pub fn alloc(&self) -> BytesMut {
        self.queue.pop().unwrap_or_else(|| {
            self.total_allocated.fetch_add(1, Ordering::Relaxed);
            BytesMut::with_capacity(BUFFER_SIZE)
        })
    }

    #[inline]
    pub fn try_alloc(&self) -> Option<BytesMut> {
        self.queue.pop().map(|buf| {
            self.total_reused.fetch_add(1, Ordering::Relaxed);
            buf
        })
    }

    #[inline]
    pub fn free(&self, mut buf: BytesMut) {
        if buf.capacity() >= BUFFER_SIZE {
            buf.clear();
            self.queue.push(buf);
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
        Self::new(DEFAULT_POOL_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool() {
        let pool = BufferPool::new(64);

        let buf1 = pool.alloc();
        assert!(buf1.is_empty());
        assert!(buf1.capacity() >= BUFFER_SIZE);

        pool.free(buf1);

        let buf2 = pool.alloc();
        assert!(buf2.is_empty());
    }
}
