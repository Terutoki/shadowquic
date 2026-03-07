use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use crossbeam::utils::CachePadded;
use tokio::sync::watch;

use crate::error::SError;
use crate::msgs::socks5::SocksAddr;
use crate::AnyUdpSend;

const SHARD_COUNT: usize = 64;
const MAX_UDP_SESSIONS: usize = 65536;

#[derive(Clone)]
pub struct UdpIdStore {
    shards: Vec<Arc<UdpIdShard>>,
}

struct UdpIdShard {
    slots: Box<[CachePadded<IdSlot>>>,
    version: CachePadded<AtomicU64>,
}

struct IdSlot {
    state: CachePadded<AtomicU32>,
    data: tokio::sync::Mutex<Option<IdData>>,
    notifier: tokio::sync::Mutex<Option<watch::Sender<()>>>,
}

struct IdData {
    udp: AnyUdpSend,
    addr: SocksAddr,
}

impl IdSlot {
    #[inline]
    fn new() -> Self {
        Self {
            state: CachePadded::new(AtomicU32::new(0)),
            data: tokio::sync::Mutex::new(None),
            notifier: tokio::sync::Mutex::new(None),
        }
    }
}

const SLOT_EMPTY: u32 = 0;
const SLOT_PENDING: u32 = 1;
const SLOT_READY: u32 = 2;
const SLOT_CLOSING: u32 = 3;

impl UdpIdStore {
    pub fn new() -> Self {
        let mut shards = Vec::with_capacity(SHARD_COUNT);
        for _ in 0..SHARD_COUNT {
            let mut slots = Vec::with_capacity(MAX_UDP_SESSIONS / SHARD_COUNT);
            for _ in 0..MAX_UDP_SESSIONS / SHARD_COUNT {
                slots.push(CachePadded::new(IdSlot::new()));
            }
            shards.push(Arc::new(UdpIdShard {
                slots: slots.into_boxed_slice(),
                version: CachePadded::new(AtomicU64::new(0)),
            }));
        }
        Self { shards }
    }

    #[inline]
    fn get_shard(&self, id: u16) -> &Arc<UdpIdShard> {
        &self.shards[(id as usize) % SHARD_COUNT]
    }

    #[inline]
    fn get_slot_index(id: u16) -> usize {
        (id as usize) / SHARD_COUNT
    }

    pub async fn get_or_create(&self, id: u16) -> Result<(AnyUdpSend, SocksAddr), SError> {
        let shard = self.get_shard(id);
        let slot_idx = Self::get_slot_index(id);
        let slot = &shard.slots[slot_idx];

        let old_state = slot.state.load(Ordering::Acquire);

        match old_state {
            SLOT_READY => {
                let data = slot.data.lock().await;
                if let Some(d) = data.as_ref() {
                    return Ok((d.udp.clone(), d.addr.clone()));
                }
            }
            SLOT_EMPTY => {
                let zero = 0u32;
                if slot.state.compare_exchange(zero, SLOT_PENDING, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                    let (tx, rx) = watch::channel(());
                    {
                        let mut notifier = slot.notifier.lock().await;
                        *notifier = Some(tx);
                    }
                    {
                        let mut data = slot.data.lock().await;
                        *data = None;
                    }
                    slot.state.store(SLOT_PENDING, Ordering::Release);
                    drop(rx);
                    return Err(SError::UDPSessionClosed("id not found".into()));
                }
            }
            SLOT_PENDING => {
                let rx = {
                    let notifier = slot.notifier.lock().await;
                    notifier.as_ref().map(|tx| tx.subscribe())
                };
                if let Some(mut rx) = rx {
                    rx.changed().await.map_err(|_| SError::UDPSessionClosed("closed".into()))?;
                    let data = slot.data.lock().await;
                    if let Some(d) = data.as_ref() {
                        return Ok((d.udp.clone(), d.addr.clone()));
                    }
                }
            }
            _ => {}
        }

        Err(SError::UDPSessionClosed("not found".into()))
    }

    pub async fn insert(&self, id: u16, udp: AnyUdpSend, addr: SocksAddr) -> Result<(), SError> {
        let shard = self.get_shard(id);
        let slot_idx = Self::get_slot_index(id);
        let slot = &shard.slots[slot_idx];

        {
            let mut data = slot.data.lock().await;
            *data = Some(IdData { udp, addr });
        }
        {
            let mut notifier = slot.notifier.lock().await;
            if let Some(tx) = notifier.take() {
                let _ = tx.send(());
            }
        }

        slot.state.store(SLOT_READY, Ordering::Release);
        shard.version.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    pub async fn try_get(&self, id: u16) -> Option<(AnyUdpSend, SocksAddr)> {
        let shard = self.get_shard(id);
        let slot_idx = Self::get_slot_index(id);
        let slot = &shard.slots[slot_idx];

        if slot.state.load(Ordering::Acquire) == SLOT_READY {
            let data = slot.data.lock().await;
            if let Some(d) = data.as_ref() {
                return Some((d.udp.clone(), d.addr.clone()));
            }
        }
        None
    }

    pub async fn remove(&self, id: u16) {
        let shard = self.get_shard(id);
        let slot_idx = Self::get_slot_index(id);
        let slot = &shard.slots[slot_idx];

        let mut data = slot.data.lock().await;
        data.take();
        slot.state.store(SLOT_EMPTY, Ordering::Release);
        shard.version.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for UdpIdStore {
    fn default() -> Self {
        Self::new()
    }
}