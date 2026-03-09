use crate::error::SError;
use bytes::Bytes;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub const MAX_BATCH_SIZE: usize = 64;
pub const DEFAULT_BATCH_SIZE: usize = 32;

#[derive(Clone, Default)]
pub struct BatchUdpStats {
    pub packets_received: Arc<AtomicUsize>,
    pub packets_sent: Arc<AtomicUsize>,
    pub recv_calls: Arc<AtomicUsize>,
    pub send_calls: Arc<AtomicUsize>,
    pub errors: Arc<AtomicUsize>,
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;
    use std::os::fd::AsRawFd;

    pub const MSG_DONTWAIT: libc::c_int = 0x40;
    const UDP_GRO: libc::c_int = 102;
    const SO_BUSY_POLL: libc::c_int = 48;
    const SO_SNDBUFFORCE: libc::c_int = 32;
    const SO_RCVBUFFORCE: libc::c_int = 33;
    const TCP_CORK: libc::c_int = 3;
    const SO_PRIORITY: libc::c_int = 12;

    pub struct BatchUdpSocket {
        fd: libc::c_int,
        local_addr: SocketAddr,
        batch_size: usize,
        running: Arc<AtomicBool>,
        stats: BatchUdpStats,
    }

    impl BatchUdpSocket {
        pub fn new(addr: SocketAddr, batch_size: usize) -> Result<Self, SError> {
            let domain = if addr.is_ipv6() {
                libc::AF_INET6
            } else {
                libc::AF_INET
            };

            let fd = unsafe { libc::socket(domain, libc::SOCK_DGRAM, libc::IPPROTO_UDP) };
            if fd < 0 {
                return Err(SError::Io(std::io::Error::last_os_error()));
            }

            let mut opt: libc::c_int = 1;
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_REUSEADDR,
                    &opt as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );

                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_REUSEPORT,
                    &opt as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }

            let sockaddr = addr_to_sockaddr(&addr);
            let ret = unsafe {
                libc::bind(
                    fd,
                    &sockaddr as *const _ as *const libc::sockaddr,
                    std::mem::size_of_val(&sockaddr) as libc::socklen_t,
                )
            };
            if ret < 0 {
                unsafe { libc::close(fd) };
                return Err(SError::Io(std::io::Error::last_os_error()));
            }

            let local_addr = get_local_addr(fd)?;

            Ok(Self {
                fd,
                local_addr,
                batch_size: batch_size.min(MAX_BATCH_SIZE),
                running: Arc::new(AtomicBool::new(true)),
                stats: BatchUdpStats::default(),
            })
        }

        pub fn set_udp_optimizations(&self) -> Result<(), SError> {
            unsafe {
                let enable: libc::c_int = 1;

                libc::setsockopt(
                    self.fd,
                    libc::IPPROTO_UDP,
                    UDP_GRO,
                    &enable as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );

                libc::setsockopt(
                    self.fd,
                    libc::SOL_SOCKET,
                    SO_BUSY_POLL,
                    &enable as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );

                let sndbuf: libc::c_int = 16 * 1024 * 1024;
                libc::setsockopt(
                    self.fd,
                    libc::SOL_SOCKET,
                    SO_SNDBUFFORCE,
                    &sndbuf as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );

                let rcvbuf: libc::c_int = 16 * 1024 * 1024;
                libc::setsockopt(
                    self.fd,
                    libc::SOL_SOCKET,
                    SO_RCVBUFFORCE,
                    &rcvbuf as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );

                libc::setsockopt(
                    self.fd,
                    libc::IPPROTO_TCP,
                    TCP_CORK,
                    &enable as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }

            Ok(())
        }

        pub fn set_congestion_control(&self, cc: &str) -> Result<(), SError> {
            if cc == "bbr" {
                unsafe {
                    let optval: libc::c_int = 1;
                    libc::setsockopt(
                        self.fd,
                        libc::IPPROTO_TCP,
                        15,
                        &optval as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    );
                }
            }
            Ok(())
        }

        pub fn set_ip_tos(&self, tos: u8) -> Result<(), SError> {
            unsafe {
                libc::setsockopt(
                    self.fd,
                    libc::IPPROTO_IP,
                    libc::IP_TOS,
                    &tos as *const _ as *const libc::c_void,
                    std::mem::size_of::<u8>() as libc::socklen_t,
                );
            }
            Ok(())
        }

        pub fn set_priority(&self, priority: u32) -> Result<(), SError> {
            unsafe {
                libc::setsockopt(
                    self.fd,
                    libc::SOL_SOCKET,
                    SO_PRIORITY,
                    &priority as *const _ as *const libc::c_void,
                    std::mem::size_of::<u32>() as libc::socklen_t,
                );
            }
            Ok(())
        }

        #[inline]
        pub fn recv_mmsg(&self, _buffer: &mut RecvMmsgBuffer) -> usize {
            0
        }

        #[inline]
        pub fn send_mmsg(&self, _packets: &[SendPacket]) -> usize {
            0
        }

        pub fn local_addr(&self) -> SocketAddr {
            self.local_addr
        }

        pub fn fd(&self) -> libc::c_int {
            self.fd
        }

        pub fn batch_size(&self) -> usize {
            self.batch_size
        }

        pub fn stop(&self) {
            self.running.store(false, Ordering::Release);
        }

        pub fn stats(&self) -> BatchUdpStats {
            self.stats.clone()
        }
    }

    impl Drop for BatchUdpSocket {
        fn drop(&mut self) {
            unsafe { libc::close(self.fd) };
        }
    }

    fn addr_to_sockaddr(addr: &SocketAddr) -> libc::sockaddr_in {
        let ip = match addr.ip() {
            std::net::Ipv4Addr(ip) => ip.octets(),
            std::net::Ipv6Addr(ip) => {
                let segs = ip.segments();
                [
                    (segs[0] >> 8) as u8 | (segs[0] as u8) << 8,
                    segs[0] as u8,
                    (segs[1] >> 8) as u8 | (segs[1] as u8) << 8,
                    segs[1] as u8,
                ]
            }
        };

        libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: addr.port().to_be(),
            sin_addr: libc::in_addr {
                s_addr: u32::from_be_bytes(ip),
            },
            sin_zero: [0; 8],
        }
    }

    fn sockaddr_to_addr(sa: &libc::sockaddr_in) -> SocketAddr {
        let ip = std::net::Ipv4Addr::from(sa.sin_addr.s_addr);
        let port = u16::from_be(sa.sin_port);
        SocketAddr::new(std::net::IpAddr::V4(ip), port)
    }

    fn get_local_addr(fd: libc::c_int) -> Result<SocketAddr, SError> {
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;

        unsafe {
            if libc::getsockname(fd, &mut addr as *mut _ as *mut libc::sockaddr, &mut len) != 0 {
                return Err(SError::Io(std::io::Error::last_os_error()));
            }
        }

        Ok(sockaddr_to_addr(&addr))
    }
}

#[cfg(not(target_os = "linux"))]
mod other_impl {
    use super::*;

    pub struct BatchUdpSocket {
        local_addr: SocketAddr,
        batch_size: usize,
        running: Arc<AtomicBool>,
        stats: BatchUdpStats,
    }

    impl BatchUdpSocket {
        pub fn new(addr: SocketAddr, batch_size: usize) -> Result<Self, SError> {
            Ok(Self {
                local_addr: addr,
                batch_size: batch_size.min(MAX_BATCH_SIZE),
                running: Arc::new(AtomicBool::new(true)),
                stats: BatchUdpStats::default(),
            })
        }

        pub fn set_udp_optimizations(&self) -> Result<(), SError> {
            Ok(())
        }

        pub fn set_congestion_control(&self, _cc: &str) -> Result<(), SError> {
            Ok(())
        }

        pub fn set_ip_tos(&self, _tos: u8) -> Result<(), SError> {
            Ok(())
        }

        pub fn set_priority(&self, _priority: u32) -> Result<(), SError> {
            Ok(())
        }

        #[inline]
        pub fn recv_mmsg(&self, _buffer: &mut RecvMmsgBuffer) -> usize {
            0
        }

        #[inline]
        pub fn send_mmsg(&self, _packets: &[SendPacket]) -> usize {
            0
        }

        pub fn local_addr(&self) -> SocketAddr {
            self.local_addr
        }

        pub fn fd(&self) -> libc::c_int {
            -1
        }

        pub fn batch_size(&self) -> usize {
            self.batch_size
        }

        pub fn stop(&self) {
            self.running.store(false, Ordering::Release);
        }

        pub fn stats(&self) -> BatchUdpStats {
            self.stats.clone()
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux_impl::BatchUdpSocket;

#[cfg(not(target_os = "linux"))]
pub use other_impl::BatchUdpSocket;

use std::sync::Arc;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Msghdr {
    pub msg_name: *mut libc::c_void,
    pub msg_namelen: libc::socklen_t,
    pub msg_iov: *mut libc::iovec,
    pub msg_iovlen: usize,
    pub msg_control: *mut libc::c_void,
    pub msg_controllen: usize,
    pub msg_flags: libc::c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Mmsghdr {
    pub msg_hdr: Msghdr,
    pub msg_len: libc::c_uint,
}

pub struct RecvMmsgBuffer {
    pub addrs: [libc::sockaddr_in; MAX_BATCH_SIZE],
    pub iovecs: [libc::iovec; MAX_BATCH_SIZE],
    pub mmsghdrs: [Mmsghdr; MAX_BATCH_SIZE],
    pub buffers: Vec<Bytes>,
    pub msg_count: usize,
}

impl RecvMmsgBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.min(MAX_BATCH_SIZE);
        let mut buffers = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffers.push(Bytes::from(vec![0u8; 65535]));
        }

        let mut addrs: [libc::sockaddr_in; MAX_BATCH_SIZE] =
            [unsafe { std::mem::zeroed() }; MAX_BATCH_SIZE];
        let mut iovecs: [libc::iovec; MAX_BATCH_SIZE] =
            [unsafe { std::mem::zeroed() }; MAX_BATCH_SIZE];
        let mut mmsghdrs: [Mmsghdr; MAX_BATCH_SIZE] =
            [unsafe { std::mem::zeroed() }; MAX_BATCH_SIZE];

        for i in 0..capacity {
            addrs[i].sin_family = libc::AF_INET as libc::sa_family_t;

            iovecs[i].iov_base = buffers[i].as_ptr() as *mut libc::c_void;
            iovecs[i].iov_len = 65535;

            mmsghdrs[i].msg_hdr.msg_name = &mut addrs[i] as *mut _ as *mut libc::c_void;
            mmsghdrs[i].msg_hdr.msg_namelen =
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            mmsghdrs[i].msg_hdr.msg_iov = &mut iovecs[i];
            mmsghdrs[i].msg_hdr.msg_iovlen = 1;
            mmsghdrs[i].msg_hdr.msg_control = std::ptr::null_mut();
            mmsghdrs[i].msg_hdr.msg_controllen = 0;
            mmsghdrs[i].msg_hdr.msg_flags = 0;
            mmsghdrs[i].msg_len = 0;
        }

        Self {
            addrs,
            iovecs,
            mmsghdrs,
            buffers,
            msg_count: 0,
        }
    }

    pub fn update_buffers(&mut self, capacity: usize) {
        for i in 0..capacity {
            self.buffers[i] = Bytes::from(vec![0u8; 65535]);
            self.iovecs[i].iov_base = self.buffers[i].as_ptr() as *mut libc::c_void;
            self.iovecs[i].iov_len = 65535;
        }
    }
}

pub struct SendMmsgBuffer {
    pub addrs: [libc::sockaddr_in; MAX_BATCH_SIZE],
    pub iovecs: [libc::iovec; MAX_BATCH_SIZE],
    pub mmsghdrs: [Mmsghdr; MAX_BATCH_SIZE],
    pub buffers: Vec<Bytes>,
    pub msg_count: usize,
}

impl SendMmsgBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.min(MAX_BATCH_SIZE);
        let buffers = Vec::with_capacity(capacity);

        let mut addrs: [libc::sockaddr_in; MAX_BATCH_SIZE] =
            [unsafe { std::mem::zeroed() }; MAX_BATCH_SIZE];
        let mut iovecs: [libc::iovec; MAX_BATCH_SIZE] =
            [unsafe { std::mem::zeroed() }; MAX_BATCH_SIZE];
        let mut mmsghdrs: [Mmsghdr; MAX_BATCH_SIZE] =
            [unsafe { std::mem::zeroed() }; MAX_BATCH_SIZE];

        for i in 0..capacity {
            addrs[i].sin_family = libc::AF_INET as libc::sa_family_t;

            mmsghdrs[i].msg_hdr.msg_name = &mut addrs[i] as *mut _ as *mut libc::c_void;
            mmsghdrs[i].msg_hdr.msg_namelen =
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            mmsghdrs[i].msg_hdr.msg_iov = &mut iovecs[i];
            mmsghdrs[i].msg_hdr.msg_iovlen = 1;
            mmsghdrs[i].msg_hdr.msg_control = std::ptr::null_mut();
            mmsghdrs[i].msg_hdr.msg_controllen = 0;
            mmsghdrs[i].msg_hdr.msg_flags = 0;
            mmsghdrs[i].msg_len = 0;
        }

        Self {
            addrs,
            iovecs,
            mmsghdrs,
            buffers,
            msg_count: 0,
        }
    }
}

#[derive(Clone)]
pub struct RecvPacket {
    pub data: Bytes,
    pub addr: SocketAddr,
}

#[derive(Clone)]
pub struct SendPacket {
    pub data: Bytes,
    pub addr: SocketAddr,
}

pub struct BatchUdpWorker {
    socket: Arc<BatchUdpSocket>,
    recv_buffer: std::sync::Mutex<RecvMmsgBuffer>,
}

impl BatchUdpWorker {
    pub fn new(addr: SocketAddr, batch_size: usize) -> Result<Self, SError> {
        let socket = Arc::new(BatchUdpSocket::new(addr, batch_size)?);
        socket.set_udp_optimizations()?;

        Ok(Self {
            socket,
            recv_buffer: std::sync::Mutex::new(RecvMmsgBuffer::new(batch_size)),
        })
    }

    pub fn poll(&self) -> Vec<RecvPacket> {
        let mut buffer = self.recv_buffer.lock().unwrap();
        let batch_size = self.socket.batch_size();
        buffer.update_buffers(batch_size);

        let count = self.socket.recv_mmsg(&mut buffer);

        let mut packets = Vec::with_capacity(count);
        for i in 0..count {
            let ip_bytes = buffer.addrs[i].sin_addr.s_addr.to_ne_bytes();
            packets.push(RecvPacket {
                data: buffer.buffers[i].clone(),
                addr: SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::from(ip_bytes)),
                    u16::from_be(buffer.addrs[i].sin_port),
                ),
            });
        }

        packets
    }

    pub fn send(&self, packets: &[SendPacket]) -> usize {
        self.socket.send_mmsg(packets)
    }

    pub fn socket(&self) -> &Arc<BatchUdpSocket> {
        &self.socket
    }

    pub fn stop(&self) {
        self.socket.stop();
    }

    pub fn stats(&self) -> BatchUdpStats {
        self.socket.stats()
    }
}
