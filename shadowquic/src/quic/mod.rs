use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::AtomicUsize;

use crate::error::SResult;
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::atomic::Ordering;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};

// 4 times larger than quinn default value
// Better decrease the size for portable device
pub const MAX_WINDOW_BASE: u64 = 4 * 12_500_000 * 100 / 1000; // 100ms RTT
pub const MAX_STREAM_WINDOW: u64 = MAX_WINDOW_BASE;
pub const MAX_SEND_WINDOW: u64 = MAX_WINDOW_BASE * 8;
pub const MAX_DATAGRAM_WINDOW: u64 = MAX_WINDOW_BASE * 2;

pub const MAX_CONCURRENT_BIDI_STREAMS: u32 = 1000;
pub const MAX_CONCURRENT_UNI_STREAMS: u32 = 1000;

pub struct Stats {
    pub total_connections: AtomicUsize,
    pub active_connections: AtomicUsize,
    pub total_packets_sent: AtomicUsize,
    pub total_packets_received: AtomicUsize,
    pub total_bytes_sent: AtomicUsize,
    pub total_bytes_received: AtomicUsize,
}

impl Stats {
    pub const fn new() -> Self {
        Self {
            total_connections: AtomicUsize::new(0),
            active_connections: AtomicUsize::new(0),
            total_packets_sent: AtomicUsize::new(0),
            total_packets_received: AtomicUsize::new(0),
            total_bytes_sent: AtomicUsize::new(0),
            total_bytes_received: AtomicUsize::new(0),
        }
    }
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

impl Stats {
    pub fn connection_opened(&self) {
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn packet_sent(&self, size: usize) {
        self.total_packets_sent.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_sent.fetch_add(size, Ordering::Relaxed);
    }

    pub fn packet_received(&self, size: usize) {
        self.total_packets_received.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_received.fetch_add(size, Ordering::Relaxed);
    }
}

pub static STATS: Stats = Stats::new();

// #[cfg(feature = "gm-quic")]
// mod gm_quic_wrapper;
// #[cfg(feature = "gm-quic")]
// pub use gm_quic_wrapper::{Connection, EndClient, EndServer, QuicErrorRepr};

#[async_trait]
pub trait QuicClient: Send + Sync {
    type C: QuicConnection;
    type SC: Clone + Send + Sync + 'static;
    async fn new(cfg: &Self::SC, ipv6: bool) -> SResult<Self>
    where
        Self: Sized;
    fn new_with_socket(cfg: &Self::SC, socket: UdpSocket) -> SResult<Self>
    where
        Self: Sized;
    async fn connect(&self, addr: SocketAddr, server_name: &str) -> Result<Self::C, QuicErrorRepr>;
}
#[async_trait]
pub trait QuicServer: Send + Sync {
    type C: QuicConnection;
    type SC: Clone + Send + Sync + 'static;
    async fn new(cfg: &Self::SC) -> SResult<Self>
    where
        Self: Sized;
    async fn accept(&self) -> Result<Self::C, QuicErrorRepr>;
}

#[async_trait]
pub trait QuicConnection: Send + Sync + Clone + 'static {
    type SendStream: AsyncWrite + Unpin + Send + Sync + 'static;
    type RecvStream: AsyncRead + Unpin + Send + Sync + 'static;
    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream, u64), QuicErrorRepr>;
    async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream, u64), QuicErrorRepr>;
    async fn open_uni(&self) -> Result<(Self::SendStream, u64), QuicErrorRepr>;
    async fn accept_uni(&self) -> Result<(Self::RecvStream, u64), QuicErrorRepr>;
    async fn read_datagram(&self) -> Result<Bytes, QuicErrorRepr>;
    async fn send_datagram(&self, bytes: Bytes) -> Result<(), QuicErrorRepr>;
    fn close(&self, error_code: u64, reason: &[u8]);
    fn close_reason(&self) -> Option<QuicErrorRepr>;
    fn remote_address(&self) -> SocketAddr;
    fn peer_id(&self) -> u64;
}

#[derive(Error, Debug)]
#[error(transparent)]
pub enum QuicErrorRepr {
    // gm-quic errors
    #[error("QUIC IO Error:{0}")]
    QuicIoError(String),
    #[error("QUIC Error:{0}")]
    QuicBaseError(String),
    #[error("Failed to build Quic Listener:{0}")]
    QuicListenerBuilderError(String),

    // quinn errors
    #[error("QUIC Connect Error:{0}")]
    QuicConnect(String),
    #[error("QUIC Connection Error:{0}")]
    QuicConnection(String),
    #[error("QUIC Write Error:{0}")]
    QuicWrite(String),
    #[error("QUIC ReadExact Error:{0}")]
    QuicReadExactError(String),
    #[error("QUIC SendDatagramError:{0}")]
    QuicSendDatagramError(String),
    #[error("JLS Authentication failed")]
    JlsAuthFailed,
}
