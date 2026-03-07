use crossbeam::queue::SegQueue;
use slab::Slab;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

const INITIAL_CAPACITY: usize = 4096;

pub struct IdAllocator<T = ()> {
    slab: Arc<parking_lot::RwLock<Slab<T>>>,
    free_list: Arc<SegQueue<u32>>,
    next_id: AtomicU32,
}

impl<T> IdAllocator<T> {
    pub fn new() -> Self {
        let mut slab = Slab::with_capacity(INITIAL_CAPACITY);
        for _ in 0..INITIAL_CAPACITY {
            slab.insert(unsafe { std::mem::zeroed() });
        }

        Self {
            slab: Arc::new(parking_lot::RwLock::new(slab)),
            free_list: Arc::new(SegQueue::new()),
            next_id: AtomicU32::new(INITIAL_CAPACITY as u32),
        }
    }

    #[inline]
    pub fn alloc(&self) -> u32 {
        if let Some(id) = self.free_list.pop() {
            return id;
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let mut slab = self.slab.write();
        if id as usize >= slab.len() {
            let new_capacity = ((id as usize) + 1024).next_power_of_two();
            let mut new_slab = Slab::with_capacity(new_capacity);
            for i in 0..new_capacity {
                new_slab.insert(unsafe { std::mem::zeroed() });
            }
            *slab = new_slab;
        }

        id
    }

    #[inline]
    pub fn free(&self, id: u32) {
        self.free_list.push(id);
    }

    pub fn len(&self) -> usize {
        self.slab.read().len()
    }
}

impl<T> Default for IdAllocator<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct ShardedIdAllocator {
    shards: Vec<Arc<IdAllocator<()>>>,
    shard_mask: usize,
}

impl ShardedIdAllocator {
    pub fn new(num_shards: usize) -> Self {
        let num_shards = num_shards.next_power_of_two();
        let shards = (0..num_shards)
            .map(|_| Arc::new(IdAllocator::new()))
            .collect();

        Self {
            shards,
            shard_mask: num_shards - 1,
        }
    }

    #[inline]
    pub fn alloc(&self, shard_key: usize) -> u32 {
        let shard = &self.shards[shard_key & self.shard_mask];
        shard.alloc()
    }

    #[inline]
    pub fn free(&self, id: u32, shard_key: usize) {
        let shard = &self.shards[shard_key & self.shard_mask];
        shard.free(id);
    }
}

pub type SessionId = u32;

pub struct SessionIdGenerator {
    counter: AtomicU32,
}

impl SessionIdGenerator {
    pub fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }

    pub fn next(&self) -> SessionId {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for SessionIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_allocator() {
        let allocator: IdAllocator<()> = IdAllocator::new();

        let id1 = allocator.alloc();
        let id2 = allocator.alloc();

        assert_ne!(id1, id2);

        allocator.free(id1);

        let id3 = allocator.alloc();
        assert_eq!(id1, id3);
    }

    #[test]
    fn test_sharded_allocator() {
        let allocator = ShardedIdAllocator::new(4);

        let _id1 = allocator.alloc(0);
        let _id2 = allocator.alloc(0);
        let _id3 = allocator.alloc(1);

        allocator.free(_id1, 0);
    }
}
