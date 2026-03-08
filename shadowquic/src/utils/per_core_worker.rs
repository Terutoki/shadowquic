use crate::error::SError;
use crate::pool::optimized_session::OptimizedSessionManager;
use crate::utils::udp_batch::PacketQueue;
use bytes::Bytes;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

#[cfg(target_os = "linux")]
use std::os::linux::thread::JoinHandleExt;

pub struct WorkerConfig {
    pub cpu_affinity: Option<Vec<usize>>,
    pub worker_threads: usize,
    pub queue_size: usize,
    pub poll_timeout_us: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            cpu_affinity: None,
            worker_threads: num_cpus(),
            queue_size: 4096,
            poll_timeout_us: 100,
        }
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
}

#[repr(align(64))]
pub struct WorkerLocal {
    id: usize,
    cpu_id: usize,
    packet_queue: PacketQueue,
    session_manager: OptimizedSessionManager,
    stats: WorkerStats,
    running: AtomicBool,
}

#[derive(Clone, Default)]
pub struct WorkerStats {
    packets_processed: Arc<AtomicUsize>,
    packets_dropped: Arc<AtomicUsize>,
    cycles_idle: Arc<AtomicUsize>,
    cycles_busy: Arc<AtomicUsize>,
}

impl WorkerLocal {
    pub fn new(id: usize, cpu_id: usize) -> Self {
        Self {
            id,
            cpu_id,
            packet_queue: PacketQueue::new(4096),
            session_manager: OptimizedSessionManager::new(),
            stats: WorkerStats::default(),
            running: AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    #[inline]
    pub fn cpu_id(&self) -> usize {
        self.cpu_id
    }

    #[inline]
    pub fn packet_queue(&self) -> &PacketQueue {
        &self.packet_queue
    }

    #[inline]
    pub fn session_manager(&self) -> &OptimizedSessionManager {
        &self.session_manager
    }

    #[inline]
    pub fn stats(&self) -> WorkerStats {
        self.stats.clone()
    }

    #[inline]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    #[inline]
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    #[inline]
    pub fn process_packet(&self, packet: (Bytes, SocketAddr, SocketAddr)) {
        self.stats.packets_processed.fetch_add(1, Ordering::Relaxed);
        self.packet_queue.push(packet);
    }

    #[inline]
    pub fn drain_queue(&self, handler: impl Fn((Bytes, SocketAddr, SocketAddr))) {
        while let Some(packet) = self.packet_queue.pop() {
            handler(packet);
        }
    }
}

pub struct PerCoreWorker {
    worker: Arc<WorkerLocal>,
    handle: Option<thread::JoinHandle<()>>,
}

impl PerCoreWorker {
    pub fn new(id: usize, cpu_id: usize) -> Self {
        let worker = Arc::new(WorkerLocal::new(id, cpu_id));

        Self {
            worker,
            handle: None,
        }
    }

    pub fn worker(&self) -> &Arc<WorkerLocal> {
        &self.worker
    }

    pub fn spawn<F>(&mut self, handler: F) -> Result<(), SError>
    where
        F: Fn(Arc<WorkerLocal>) + Send + 'static,
    {
        let worker = Arc::clone(&self.worker);
        self.worker.running.store(true, Ordering::Release);

        let handle = thread::Builder::new()
            .name(format!("worker-{}", self.worker.id))
            .spawn(move || {
                #[cfg(target_os = "linux")]
                if let Ok(tid) = thread::current().tid() {
                    unsafe {
                        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
                        let cpu_id = worker.cpu_id();
                        libc::CPU_SET(cpu_id, &mut cpuset);
                        libc::sched_setaffinity(
                            tid as libc::pid_t,
                            std::mem::size_of::<libc::cpu_set_t>(),
                            &cpuset,
                        );
                    }
                }

                handler(worker);
            })
            .map_err(|e| {
                SError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })?;

        self.handle = Some(handle);
        Ok(())
    }

    pub fn stop(&self) {
        self.worker.stop();
    }
}

pub struct WorkerPool {
    workers: Vec<PerCoreWorker>,
    config: WorkerConfig,
}

impl WorkerPool {
    pub fn new(config: WorkerConfig) -> Self {
        let cpu_count = num_cpus();
        let worker_count = config.worker_threads.min(cpu_count);

        let mut workers = Vec::with_capacity(worker_count);

        for i in 0..worker_count {
            let cpu_id = config
                .cpu_affinity
                .as_ref()
                .map(|c| c[i % c.len()])
                .unwrap_or(i % cpu_count);

            workers.push(PerCoreWorker::new(i, cpu_id));
        }

        Self { workers, config }
    }

    pub fn start<F>(&mut self, handler: F) -> Result<(), SError>
    where
        F: Fn(Arc<WorkerLocal>) + Send + Clone + 'static,
    {
        for worker in &mut self.workers {
            let handler_clone = handler.clone();
            worker.spawn(move |local| {
                handler_clone(local);
            })?;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        for worker in &self.workers {
            worker.stop();
        }
    }

    #[inline]
    pub fn get_worker(&self, addr: &SocketAddr) -> &Arc<WorkerLocal> {
        let mut hasher = rustc_hash::FxHasher::default();
        addr.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % self.workers.len();
        self.workers[idx].worker()
    }

    #[inline]
    pub fn get_worker_by_id(&self, id: usize) -> &Arc<WorkerLocal> {
        &self.workers[id % self.workers.len()].worker()
    }

    pub fn stats(&self) -> PoolStats {
        let mut stats = PoolStats::default();

        for worker in &self.workers {
            let ws = worker.worker().stats();
            stats.total_processed += ws.packets_processed.load(Ordering::Relaxed);
            stats.total_dropped += ws.packets_dropped.load(Ordering::Relaxed);
            stats.worker_count += 1;
        }

        stats
    }
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::new(WorkerConfig::default())
    }
}

#[derive(Debug, Default)]
pub struct PoolStats {
    pub worker_count: usize,
    pub total_processed: usize,
    pub total_dropped: usize,
}

use std::hash::{Hash, Hasher};

pub struct FlowKey {
    src: SocketAddr,
    dst: SocketAddr,
}

impl FlowKey {
    pub fn new(src: SocketAddr, dst: SocketAddr) -> Self {
        Self { src, dst }
    }
}

impl PartialEq for FlowKey {
    fn eq(&self, other: &Self) -> bool {
        self.src == other.src && self.dst == other.dst
    }
}

impl Eq for FlowKey {}

impl Hash for FlowKey {
    fn hash<H: Hasher>(&self, H: &mut H) {
        self.src.hash(H);
        self.dst.hash(H);
    }
}

pub struct FlowDistributor {
    workers: Arc<Vec<Arc<WorkerLocal>>>,
    mask: usize,
}

impl FlowDistributor {
    pub fn new(workers: Vec<Arc<WorkerLocal>>) -> Self {
        let mask = workers.len().next_power_of_two() - 1;
        Self {
            workers: Arc::new(workers),
            mask,
        }
    }

    #[inline]
    pub fn get_worker(&self, flow: &FlowKey) -> &Arc<WorkerLocal> {
        let mut hasher = rustc_hash::FxHasher::default();
        flow.hash(&mut hasher);
        let idx = (hasher.finish() as usize) & self.mask;
        &self.workers[idx]
    }

    #[inline]
    pub fn get_worker_by_addr(&self, addr: &SocketAddr) -> &Arc<WorkerLocal> {
        let mut hasher = rustc_hash::FxHasher::default();
        addr.hash(&mut hasher);
        let idx = (hasher.finish() as usize) & self.mask;
        &self.workers[idx]
    }
}

pub fn set_cpu_affinity(cpu_ids: &[usize]) -> Result<(), SError> {
    #[cfg(target_os = "linux")]
    {
        let pid = std::process::id();
        unsafe {
            let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
            for &cpu_id in cpu_ids {
                libc::CPU_SET(cpu_id, &mut cpuset);
            }
            if libc::sched_setaffinity(
                pid as libc::pid_t,
                std::mem::size_of::<libc::cpu_set_t>(),
                &cpuset,
            ) != 0
            {
                return Err(SError::Io(std::io::Error::last_os_error()));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = cpu_ids;
    }

    Ok(())
}
