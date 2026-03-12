use bytes::BytesMut;
use once_cell::sync::Lazy;

use crate::BufferPool;

static GLOBAL_BUFFER_POOL: Lazy<BufferPool> = Lazy::new(|| BufferPool::new(4096));
static LARGE_BUFFER_POOL: Lazy<BufferPool> = Lazy::new(|| BufferPool::new(1024));

#[inline]
pub fn alloc_buffer() -> BytesMut {
    GLOBAL_BUFFER_POOL.alloc()
}

#[inline]
pub fn free_buffer(buf: BytesMut) {
    GLOBAL_BUFFER_POOL.free(buf)
}

#[inline]
pub fn alloc_large_buffer() -> BytesMut {
    LARGE_BUFFER_POOL.alloc()
}

#[inline]
pub fn free_large_buffer(buf: BytesMut) {
    LARGE_BUFFER_POOL.free(buf)
}
