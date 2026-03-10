#![allow(dead_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use bytes::Bytes;
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};

pub const BATCH_SIZE: usize = 32;
pub const CACHE_LINE_SIZE: usize = 64;

#[derive(Clone)]
pub struct PacketBatch {
    entries: Vec<BatchEntry>,
    count: usize,
}

#[derive(Clone)]
pub struct BatchEntry {
    pub data: Bytes,
    pub src_addr: SocketAddr,
    pub dst_addr: SocketAddr,
}

impl PacketBatch {
    #[inline]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            count: 0,
        }
    }

    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self::new(capacity)
    }

    #[inline]
    pub fn push(&mut self, entry: BatchEntry) -> bool {
        if self.entries.len() < BATCH_SIZE {
            self.entries.push(entry);
            self.count += 1;
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn push_unchecked(&mut self, entry: BatchEntry) {
        self.entries.push(entry);
        self.count += 1;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.count >= BATCH_SIZE
    }

    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.count = 0;
    }

    #[inline]
    pub fn entries(&self) -> &[BatchEntry] {
        &self.entries[..self.count]
    }

    #[inline]
    pub fn entries_mut(&mut self) -> &mut [BatchEntry] {
        &mut self.entries[..self.count]
    }
}

impl Deref for PacketBatch {
    type Target = [BatchEntry];

    fn deref(&self) -> &Self::Target {
        self.entries()
    }
}

impl DerefMut for PacketBatch {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.entries_mut()
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
mod simd {
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;

    #[target_feature(enable = "sse4.1")]
    #[inline]
    pub unsafe fn batch_copy_sse(src: &[u8], dst: &mut [u8], len: usize) {
        let mut i = 0;
        let vec_size = 16;

        while i + vec_size <= len {
            let loaded = _mm_loadu_si128(src.as_ptr().add(i) as *const __m128i);
            _mm_storeu_si128(dst.as_mut_ptr().add(i) as *mut __m128i, loaded);
            i += vec_size;

            let remainder = len - i;
            if remainder >= vec_size {
                continue;
            }
            for j in 0..remainder {
                dst[i + j] = src[i + j];
            }
        }
    }

    #[target_feature(enable = "sse4.1")]
    #[inline]
    pub unsafe fn find_header_sse(data: &[u8], header: u8) -> Option<usize> {
        let mut i = 0;
        let vec_size = 16;
        let header_vec = _mm_set1_epi8(header as i8);

        while i + vec_size <= data.len() {
            let loaded = _mm_loadu_si128(data.as_ptr().add(i) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(loaded, header_vec);
            let mask = _mm_movemask_epi8(cmp);

            if mask != 0 {
                return Some(i + mask.trailing_zeros() as usize);
            }
            i += vec_size;
        }

        for j in i..data.len() {
            if data[j] == header {
                return Some(j);
            }
        }
        None
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn batch_copy_avx(src: &[u8], dst: &mut [u8], len: usize) {
        let mut i = 0;
        let vec_size = 32;

        while i + vec_size <= len {
            let loaded = _mm256_loadu_si256(src.as_ptr().add(i) as *const __m256i);
            _mm256_storeu_si256(dst.as_mut_ptr().add(i) as *mut __m256i, loaded);
            i += vec_size;
        }

        for j in 0..len - i {
            dst[i + j] = src[i + j];
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod simd {
    use std::arch::aarch64::*;

    #[inline]
    pub unsafe fn batch_copy_neon(src: &[u8], dst: &mut [u8], len: usize) {
        let mut i = 0;
        let vec_size = 16;

        while i + vec_size <= len {
            let loaded = vld1q_u8(src.as_ptr().add(i));
            vst1q_u8(dst.as_mut_ptr().add(i), loaded);
            i += vec_size;
        }

        for j in 0..len - i {
            dst[i + j] = src[i + j];
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
mod simd {
    #[inline]
    pub unsafe fn batch_copy_fallback(src: &[u8], dst: &mut [u8], len: usize) {
        dst[..len].copy_from_slice(&src[..len]);
    }
}

pub use simd::*;

pub struct BatchProcessor {
    buffer: Vec<u8>,
    temp_buffer: Vec<u8>,
}

impl BatchProcessor {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            temp_buffer: Vec::with_capacity(capacity),
        }
    }

    #[inline]
    pub fn process_batch(&mut self, batch: &mut PacketBatch) -> usize {
        let mut processed = 0;

        for entry in batch.entries_mut() {
            if let Some(offset) = self.find_header(&entry.data, 0) {
                processed += 1;
            }
        }

        processed
    }

    #[inline]
    fn find_header(&self, data: &[u8], header: u8) -> Option<usize> {
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        {
            if is_x86_feature_detected!("sse4.1") {
                unsafe {
                    return find_header_sse(data, header);
                }
            }
        }

        data.iter().position(|&b| b == header)
    }

    #[inline]
    pub fn copy_batch(src: &[u8], dst: &mut [u8], len: usize) {
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    return simd::batch_copy_avx(src, dst, len);
                }
            }
            if is_x86_feature_detected!("sse4.1") {
                unsafe {
                    return simd::batch_copy_sse(src, dst, len);
                }
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            unsafe {
                simd::batch_copy_neon(src, dst, len);
                return;
            }
        }

        dst[..len].copy_from_slice(&src[..len]);
    }
}

impl Default for BatchProcessor {
    fn default() -> Self {
        Self::new(65536)
    }
}

pub struct AtomicBatch<T: Clone> {
    slots: Vec<CacheAlignedSlot<T>>,
    head: std::sync::atomic::AtomicUsize,
    tail: std::sync::atomic::AtomicUsize,
    capacity: usize,
}

struct CacheAlignedSlot<T> {
    data: std::sync::atomic::AtomicPtr<T>,
    ready: std::sync::atomic::AtomicBool,
}

impl<T: Clone> AtomicBatch<T> {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two();
        let mut slots = Vec::with_capacity(capacity);

        for _ in 0..capacity {
            slots.push(CacheAlignedSlot {
                data: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
                ready: std::sync::atomic::AtomicBool::new(false),
            });
        }

        Self {
            slots,
            head: std::sync::atomic::AtomicUsize::new(0),
            tail: std::sync::atomic::AtomicUsize::new(0),
            capacity,
        }
    }

    #[inline]
    pub fn push(&self, item: T) -> bool {
        let tail = self.tail.load(std::sync::atomic::Ordering::Acquire);
        let head = self.head.load(std::sync::atomic::Ordering::Acquire);

        if (tail.wrapping_sub(head)) >= self.capacity {
            return false;
        }

        let index = tail & (self.capacity - 1);
        let boxed = Box::new(item);
        self.slots[index].data.store(
            Box::into_raw(boxed) as *mut T,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.slots[index]
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        self.tail
            .store(tail.wrapping_add(1), std::sync::atomic::Ordering::Release);

        true
    }

    #[inline]
    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(std::sync::atomic::Ordering::Acquire);
        let tail = self.tail.load(std::sync::atomic::Ordering::Acquire);

        if head == tail {
            return None;
        }

        let index = head & (self.capacity - 1);

        if !self.slots[index]
            .ready
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }

        let ptr = self.slots[index]
            .data
            .load(std::sync::atomic::Ordering::Acquire);
        let item = unsafe { *Box::from_raw(ptr) };
        self.slots[index]
            .ready
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.head
            .store(head.wrapping_add(1), std::sync::atomic::Ordering::Release);

        Some(item)
    }

    #[inline]
    pub fn len(&self) -> usize {
        let tail = self.tail.load(std::sync::atomic::Ordering::Acquire);
        let head = self.head.load(std::sync::atomic::Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

unsafe impl<T: Clone> Send for AtomicBatch<T> {}
unsafe impl<T: Clone> Sync for AtomicBatch<T> {}
