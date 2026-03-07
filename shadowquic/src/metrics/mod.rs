use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Default)]
pub struct Metrics {
    pub packets_received: Arc<AtomicU64>,
    pub packets_sent: Arc<AtomicU64>,
    pub bytes_received: Arc<AtomicU64>,
    pub bytes_sent: Arc<AtomicU64>,
    pub connections_active: Arc<AtomicUsize>,
    pub connections_total: Arc<AtomicUsize>,
    pub errors: Arc<AtomicUsize>,
    pub udp_sessions: Arc<AtomicUsize>,
    pub tcp_sessions: Arc<AtomicUsize>,
}

impl Metrics {
    #[inline(always)]
    pub fn record_packet_received(&self, size: usize) {
        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(size as u64, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_packet_sent(&self, size: usize) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(size as u64, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_connection(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
        self.connections_active.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn close_connection(&self) {
        self.connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_udp_session(&self) {
        self.udp_sessions.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tcp_session(&self) {
        self.tcp_sessions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            connections_active: self.connections_active.load(Ordering::Relaxed),
            connections_total: self.connections_total.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            udp_sessions: self.udp_sessions.load(Ordering::Relaxed),
            tcp_sessions: self.tcp_sessions.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub packets_received: u64,
    pub packets_sent: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub connections_active: usize,
    pub connections_total: usize,
    pub errors: usize,
    pub udp_sessions: usize,
    pub tcp_sessions: usize,
}

pub static GLOBAL_METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::default);

#[macro_export]
macro_rules! metric_increment {
    ($name:ident) => {
        $name.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    };
}

#[macro_export]
macro_rules! metric_add {
    ($name:ident, $value:expr) => {
        $name.fetch_add($value as u64, std::sync::atomic::Ordering::Relaxed);
    };
}
