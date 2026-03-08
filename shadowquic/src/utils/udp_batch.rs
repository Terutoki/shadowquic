use crate::error::SError;
use bytes::Bytes;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::unix::io::{AsRawFd, RawFd};

const BATCH_SIZE: usize = 64;

pub struct UdpBatchSocket {
    socket: Socket,
    addr: SocketAddr,
}

impl UdpBatchSocket {
    pub fn new(addr: SocketAddr) -> Result<Self, SError> {
        let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
        
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        
        #[cfg(any(target_os = "linux", target_os = "android"))]
        socket.set_reuse_port(true)?;
        
        socket.bind(&addr.into())?;
        
        Ok(Self { socket, addr })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, SError> {
        self.socket.local_addr().map(|a| a.as_socket().unwrap()).map_err(|e| SError::Io(e))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}

use tokio::net::UdpSocket;

pub struct OptimizedUdpSession {
    socket: UdpSocket,
    peer_addr: SocketAddr,
}

impl OptimizedUdpSession {
    pub fn new(socket: UdpSocket, peer_addr: SocketAddr) -> Self {
        Self { socket, peer_addr }
    }

    pub async fn recv_from(&mut self) -> Result<(Bytes, SocketAddr), SError> {
        let mut buf = vec![0u8; 65535];
        let (len, addr) = self.socket.recv_from(&mut buf).await?;
        buf.truncate(len);
        Ok((Bytes::from(buf), addr))
    }

    pub async fn send_to(&self, buf: Bytes, addr: SocketAddr) -> Result<usize, SError> {
        let len = self.socket.send_to(&buf, addr).await?;
        Ok(len)
    }
}

pub struct UdpWorker {
    socket: Arc<UdpBatchSocket>,
    running: Arc<AtomicBool>,
}

impl UdpWorker {
    pub fn new(addr: SocketAddr) -> Result<Self, SError> {
        Ok(Self {
            socket: Arc::new(UdpBatchSocket::new(addr)?),
            running: Arc::new(AtomicBool::new(true)),
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    pub fn socket(&self) -> &Arc<UdpBatchSocket> {
        &self.socket
    }
}

use crossbeam::queue::SegQueue;

pub struct PacketQueue {
    queue: SegQueue<(Bytes, SocketAddr, SocketAddr)>,
    stats: QueueStats,
}

#[derive(Clone, Default)]
pub struct QueueStats {
    pub packets_sent: Arc<AtomicUsize>,
    pub packets_received: Arc<AtomicUsize>,
    pub bytes_sent: Arc<AtomicUsize>,
    pub bytes_received: Arc<AtomicUsize>,
    pub drops: Arc<AtomicUsize>,
}

impl PacketQueue {
    pub fn new(_capacity: usize) -> Self {
        Self {
            queue: SegQueue::new(),
            stats: QueueStats::default(),
        }
    }

    #[inline]
    pub fn push(&self, packet: (Bytes, SocketAddr, SocketAddr)) {
        let _ = self.queue.push(packet);
    }

    #[inline]
    pub fn pop(&self) -> Option<(Bytes, SocketAddr, SocketAddr)> {
        self.queue.pop()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

pub struct SocketOptimizer;

impl SocketOptimizer {
    #[cfg(target_os = "linux")]
    pub fn set_udp_optimizations(socket: &Socket) -> Result<(), SError> {
        use std::os::fd::AsRawFd;
        
        let fd = socket.as_raw_fd();
        
        unsafe {
            let enable: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::IPPROTO_UDP,
                libc::UDP_GRO,
                &enable as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )?;
            
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BUSY_POLL,
                &enable as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )?;
        }
        
        socket.set_nonblocking(true)?;
        
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn set_udp_optimizations(socket: &Socket) -> Result<(), SError> {
        socket.set_nonblocking(true)?;
        Ok(())
    }

    pub fn set_tcp_optimizations(socket: &Socket) -> Result<(), SError> {
        socket.set_tcp_nodelay(true)?;
        
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            
            let fd = socket.as_raw_fd();
            
            unsafe {
                let enable: libc::c_int = 1;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_QUICKACK,
                    &enable as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                )?;
                
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_CORK,
                    &enable as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                )?;
            }
        }
        
        Ok(())
    }
}
