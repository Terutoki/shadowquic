pub mod optimized;

pub use optimized::{
    packet_buf, packet_buf_large, packet_buf_put, packet_buf_sized, ArenaStats, PacketArena,
    PacketArenaOptimized, PACKET_ARENA, PACKET_ARENA_OPTIMIZED,
};
