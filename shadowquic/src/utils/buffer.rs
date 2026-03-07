use bytes::BytesMut;
use lazy_static::lazy_static;

use crate::BufferPool;

lazy_static! {
    pub static ref GLOBAL_BUFFER_POOL: BufferPool = BufferPool::new(4096);
    pub static ref LARGE_BUFFER_POOL: BufferPool = BufferPool::new(1024);
}

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
