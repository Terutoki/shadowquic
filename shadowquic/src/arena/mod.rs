pub mod optimized;

pub use optimized::{
    ArenaStats, PACKET_ARENA, PACKET_ARENA_OPTIMIZED, PacketArena, PacketArenaOptimized,
    packet_buf, packet_buf_large, packet_buf_put, packet_buf_sized,
};
