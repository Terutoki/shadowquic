use bytes::Bytes;
use std::cell::UnsafeCell;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const RING_SIZE: usize = 4096;
pub const RING_MASK: usize = RING_SIZE - 1;

pub struct LockFreeRingBuffer {
    buffer: UnsafeCell<Vec<RingEntry>>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
    stats: RingStats,
}

#[derive(Clone, Default)]
pub struct RingStats {
    pub pushed: Arc<AtomicUsize>,
    pub popped: Arc<AtomicUsize>,
    pub overflows: Arc<AtomicUsize>,
    pub underflows: Arc<AtomicUsize>,
}

unsafe impl Send for LockFreeRingBuffer {}
unsafe impl Sync for LockFreeRingBuffer {}

#[derive(Clone)]
pub struct RingEntry {
    pub data: Bytes,
    pub src_addr: SocketAddr,
    pub dst_addr: SocketAddr,
}

impl LockFreeRingBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two();
        let mut buffer = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffer.push(RingEntry {
                data: Bytes::new(),
                src_addr: "0.0.0.0:0".parse().unwrap(),
                dst_addr: "0.0.0.0:0".parse().unwrap(),
            });
        }

        Self {
            buffer: UnsafeCell::new(buffer),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            capacity,
            stats: RingStats::default(),
        }
    }

    #[inline]
    pub fn push(&self, entry: RingEntry) -> bool {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);

        if (tail.wrapping_sub(head) & RING_MASK) >= self.capacity - 1 {
            self.stats.overflows.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        unsafe {
            unsafe {
                (&mut *self.buffer.get())[tail & RING_MASK] = entry;
            }
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);

        self.stats.pushed.fetch_add(1, Ordering::Relaxed);
        true
    }

    #[inline]
    pub fn pop(&self) -> Option<RingEntry> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);

        if head == tail {
            self.stats.underflows.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        let entry = unsafe { (&mut *self.buffer.get())[head & RING_MASK].clone() };
        self.head.store(head.wrapping_add(1), Ordering::Release);

        self.stats.popped.fetch_add(1, Ordering::Relaxed);
        Some(entry)
    }

    #[inline]
    pub fn try_push(&self, entry: RingEntry) -> bool {
        self.push(entry)
    }

    #[inline]
    pub fn try_pop(&self) -> Option<RingEntry> {
        self.pop()
    }

    #[inline]
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head) & RING_MASK
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity - 1
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn stats(&self) -> RingStats {
        self.stats.clone()
    }

    pub fn clear(&self) {
        self.head.store(0, Ordering::Release);
        self.tail.store(0, Ordering::Release);
    }
}

pub struct MpscRingBuffer {
    buffer: UnsafeCell<Vec<Option<RingEntry>>>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

unsafe impl Send for MpscRingBuffer {}
unsafe impl Sync for MpscRingBuffer {}

impl MpscRingBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two();
        let mut buffer = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffer.push(None);
        }

        Self {
            buffer: UnsafeCell::new(buffer),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            capacity,
        }
    }

    #[inline]
    pub fn push(&self, entry: RingEntry) -> bool {
        let mut tail = self.tail.load(Ordering::Acquire);
        loop {
            let next = tail.wrapping_add(1);
            let head = self.head.load(Ordering::Acquire);

            if next.wrapping_sub(head) & RING_MASK >= self.capacity {
                return false;
            }

            match self
                .tail
                .compare_exchange_weak(tail, next, Ordering::Release, Ordering::Acquire)
            {
                Ok(_) => {
                    unsafe {
                        (&mut *self.buffer.get())[tail & RING_MASK] = Some(entry);
                    }
                    return true;
                }
                Err(t) => tail = t,
            }
        }
    }

    #[inline]
    pub fn pop(&self) -> Option<RingEntry> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);

        if head == tail {
            return None;
        }

        let entry = unsafe { (&mut *self.buffer.get())[head & RING_MASK].take() };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        entry
    }

    #[inline]
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head) & RING_MASK
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

pub struct SpscRingBuffer {
    buffer: UnsafeCell<Vec<Option<RingEntry>>>,
    read_idx: AtomicUsize,
    write_idx: AtomicUsize,
    capacity: usize,
}

unsafe impl Send for SpscRingBuffer {}
unsafe impl Sync for SpscRingBuffer {}

impl SpscRingBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two();
        let mut buffer = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffer.push(None);
        }

        Self {
            buffer: UnsafeCell::new(buffer),
            read_idx: AtomicUsize::new(0),
            write_idx: AtomicUsize::new(0),
            capacity,
        }
    }

    #[inline]
    pub fn push(&self, entry: RingEntry) -> bool {
        let write = self.write_idx.load(Ordering::Acquire);
        let read = self.read_idx.load(Ordering::Acquire);

        if write.wrapping_sub(read) >= self.capacity {
            return false;
        }

        unsafe {
            (&mut *self.buffer.get())[write & RING_MASK] = Some(entry);
        }
        self.write_idx
            .store(write.wrapping_add(1), Ordering::Release);
        true
    }

    #[inline]
    pub fn pop(&self) -> Option<RingEntry> {
        let read = self.read_idx.load(Ordering::Acquire);
        let write = self.write_idx.load(Ordering::Acquire);

        if read == write {
            return None;
        }

        let entry = unsafe { (&mut *self.buffer.get())[read & RING_MASK].take() };
        self.read_idx.store(read.wrapping_add(1), Ordering::Release);
        entry
    }

    #[inline]
    pub fn len(&self) -> usize {
        let write = self.write_idx.load(Ordering::Acquire);
        let read = self.read_idx.load(Ordering::Acquire);
        write.wrapping_sub(read) & RING_MASK
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

pub struct BatchRingBuffer {
    ring: Arc<LockFreeRingBuffer>,
    batch_size: usize,
}

impl BatchRingBuffer {
    pub fn new(capacity: usize, batch_size: usize) -> Self {
        Self {
            ring: Arc::new(LockFreeRingBuffer::new(capacity)),
            batch_size,
        }
    }

    #[inline]
    pub fn push_batch(&self, entries: Vec<RingEntry>) -> usize {
        let mut pushed = 0;
        for entry in entries {
            if self.ring.push(entry) {
                pushed += 1;
            } else {
                break;
            }
        }
        pushed
    }

    #[inline]
    pub fn pop_batch(&self) -> Vec<RingEntry> {
        let mut batch = Vec::with_capacity(self.batch_size);
        for _ in 0..self.batch_size {
            if let Some(entry) = self.ring.pop() {
                batch.push(entry);
            } else {
                break;
            }
        }
        batch
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

pub struct PacketBatch {
    packets: Vec<RingEntry>,
    capacity: usize,
    src_addr: SocketAddr,
}

impl PacketBatch {
    pub fn new(capacity: usize) -> Self {
        Self {
            packets: Vec::with_capacity(capacity),
            capacity,
            src_addr: "0.0.0.0:0".parse().unwrap(),
        }
    }

    #[inline]
    pub fn push(&mut self, entry: RingEntry) -> bool {
        if self.packets.is_empty() {
            self.src_addr = entry.src_addr;
        }

        if entry.src_addr != self.src_addr && !self.packets.is_empty() {
            return false;
        }

        if self.packets.len() < self.capacity {
            self.packets.push(entry);
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn packets(&self) -> &[RingEntry] {
        &self.packets
    }

    #[inline]
    pub fn clear(&mut self) {
        self.packets.clear();
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn src_addr(&self) -> SocketAddr {
        self.src_addr
    }
}
