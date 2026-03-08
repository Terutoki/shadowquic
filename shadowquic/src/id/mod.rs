use crossbeam::queue::SegQueue;
use slab::Slab;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;

pub mod optimized;

const INITIAL_CAPACITY: usize = 4096;

pub trait IdGenerator: sealed::Sealed {
    fn next(&self) -> u32;
    fn free(&self, id: u32);
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::AtomicIdGenerator {}
    impl Sealed for super::ShardedIdGenerator {}
}

pub struct AtomicIdGenerator {
    counter: AtomicU32,
    _slab: Arc<parking_lot::RwLock<Slab<()>>>,
    free_list: Arc<SegQueue<u32>>,
}

impl Default for AtomicIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl AtomicIdGenerator {
    pub fn new() -> Self {
        let mut slab = Slab::with_capacity(INITIAL_CAPACITY);
        for _ in 0..INITIAL_CAPACITY {
            slab.insert(());
        }

        Self {
            counter: AtomicU32::new(INITIAL_CAPACITY as u32),
            _slab: Arc::new(parking_lot::RwLock::new(slab)),
            free_list: Arc::new(SegQueue::new()),
        }
    }

    #[inline(always)]
    pub fn next(&self) -> u32 {
        if let Some(id) = self.free_list.pop() {
            return id;
        }
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn free(&self, id: u32) {
        self.free_list.push(id);
    }
}

pub struct ShardedIdGenerator {
    shards: Vec<AtomicIdGenerator>,
    mask: usize,
}

impl ShardedIdGenerator {
    pub fn new(num_shards: usize) -> Self {
        let num = num_shards.next_power_of_two();
        let mut shards = Vec::with_capacity(num);
        for _ in 0..num {
            shards.push(AtomicIdGenerator::new());
        }

        Self {
            shards,
            mask: num - 1,
        }
    }

    #[inline(always)]
    pub fn next(&self, key: usize) -> u32 {
        self.shards[key & self.mask].next()
    }

    #[inline(always)]
    pub fn free(&self, key: usize, id: u32) {
        self.shards[key & self.mask].free(id)
    }
}

impl Default for ShardedIdGenerator {
    fn default() -> Self {
        Self::new(4)
    }
}

static UDP_ID_GENERATOR: LazyLock<AtomicIdGenerator> = LazyLock::new(AtomicIdGenerator::new);

#[inline(always)]
pub fn next_udp_id() -> u32 {
    UDP_ID_GENERATOR.next()
}

#[inline(always)]
pub fn free_udp_id(id: u32) {
    UDP_ID_GENERATOR.free(id)
}

use parking_lot::RwLock;

pub struct IdSlot<T> {
    data: RwLock<Option<T>>,
    version: AtomicU32,
}

impl<T> IdSlot<T> {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(None),
            version: AtomicU32::new(0),
        }
    }

    #[inline(always)]
    pub fn set(&self, value: T) {
        *self.data.write() = Some(value);
        self.version.fetch_add(1, Ordering::Release);
    }

    #[inline(always)]
    pub fn get(&self) -> Option<T>
    where
        T: Clone,
    {
        self.data.read().as_ref().cloned()
    }

    #[inline(always)]
    pub fn take(&self) -> Option<T> {
        let mut data = self.data.write();
        let val = data.take();
        if val.is_some() {
            self.version.fetch_add(1, Ordering::Release);
        }
        val
    }

    #[inline(always)]
    pub fn version(&self) -> u32 {
        self.version.load(Ordering::Acquire)
    }
}

impl<T> Default for IdSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_generator() {
        let generator = AtomicIdGenerator::new();

        let id1 = generator.next();
        let id2 = generator.next();

        assert_ne!(id1, id2);

        generator.free(id1);

        let id3 = generator.next();
        assert_eq!(id1, id3);
    }

    #[test]
    fn test_sharded() {
        let generator = ShardedIdGenerator::new(4);

        let id1 = generator.next(0);
        generator.free(0, id1);
        let id1_reused = generator.next(0);
        assert_eq!(id1, id1_reused);

        let id2 = generator.next(1);
        generator.free(1, id2);
        let id2_reused = generator.next(1);
        assert_eq!(id2, id2_reused);
    }
}
