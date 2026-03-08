use crossbeam::queue::SegQueue;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

const INITIAL_CAPACITY: usize = 4096;

pub struct LockFreeIdGenerator {
    counter: AtomicU32,
    free_list: Arc<SegQueue<u32>>,
}

impl LockFreeIdGenerator {
    pub fn new() -> Self {
        let free_list = Arc::new(SegQueue::new());

        Self {
            counter: AtomicU32::new(INITIAL_CAPACITY as u32),
            free_list,
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

impl Default for LockFreeIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ShardedIdGeneratorOptimized {
    shards: Vec<Arc<LockFreeIdGenerator>>,
    mask: usize,
}

impl ShardedIdGeneratorOptimized {
    pub fn new(num_shards: usize) -> Self {
        let num = num_shards.next_power_of_two();
        let shards = (0..num)
            .map(|_| Arc::new(LockFreeIdGenerator::new()))
            .collect();

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

impl Default for ShardedIdGeneratorOptimized {
    fn default() -> Self {
        Self::new(4)
    }
}

use std::sync::LazyLock;

static UDP_ID_GEN: LazyLock<ShardedIdGeneratorOptimized> =
    LazyLock::new(|| ShardedIdGeneratorOptimized::new(8));

#[inline(always)]
pub fn next_udp_id_optimized() -> u32 {
    UDP_ID_GEN.next(0)
}

#[inline(always)]
pub fn free_udp_id_optimized(id: u32) {
    UDP_ID_GEN.free(0, id)
}

use crossbeam::utils::CachePadded;

#[repr(align(64))]
pub struct PaddedAtomicU64 {
    value: CachePadded<AtomicU64>,
}

impl PaddedAtomicU64 {
    pub fn new(val: u64) -> Self {
        Self {
            value: CachePadded::new(AtomicU64::new(val)),
        }
    }

    #[inline]
    pub fn load(&self, order: Ordering) -> u64 {
        self.value.load(order)
    }

    #[inline]
    pub fn store(&self, val: u64, order: Ordering) {
        self.value.store(val, order);
    }

    #[inline]
    pub fn fetch_add(&self, val: u64, order: Ordering) -> u64 {
        self.value.fetch_add(val, order)
    }

    #[inline]
    pub fn fetch_sub(&self, val: u64, order: Ordering) -> u64 {
        self.value.fetch_sub(val, order)
    }

    #[inline]
    pub fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.value.compare_exchange(current, new, success, failure)
    }
}

impl Default for PaddedAtomicU64 {
    fn default() -> Self {
        Self::new(0)
    }
}

pub struct PerCpuCounter {
    counters: Vec<PaddedAtomicU64>,
    mask: usize,
}

impl PerCpuCounter {
    pub fn new(num_counters: usize) -> Self {
        let num = num_counters.next_power_of_two();
        let mut counters = Vec::with_capacity(num);
        for _ in 0..num {
            counters.push(PaddedAtomicU64::new(0));
        }
        Self {
            counters,
            mask: num - 1,
        }
    }

    #[inline]
    pub fn increment(&self, cpu_id: usize) {
        self.counters[cpu_id & self.mask].fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn get(&self, cpu_id: usize) -> u64 {
        self.counters[cpu_id & self.mask].load(Ordering::Relaxed)
    }

    #[inline]
    pub fn sum(&self) -> u64 {
        self.counters
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    #[inline]
    pub fn reset(&self, cpu_id: usize) {
        self.counters[cpu_id & self.mask].store(0, Ordering::Relaxed);
    }

    #[inline]
    pub fn reset_all(&self) {
        for c in &self.counters {
            c.store(0, Ordering::Relaxed);
        }
    }
}
