use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicU16;

use rustc_hash::FxHashMap;
use tokio::sync::{
    SetOnce,
    mpsc::{Receiver, Sender, channel},
};
use tracing::{Instrument, error, trace_span};

use crate::{
    Inbound, ProxyRequest,
    config::SunnyQuicServerCfg,
    error::SError,
    quic::QuicConnection,
    squic::inbound::{SQServerConn, SunnyQuicUsers},
    sunnyquic::EndServer,
};

use crate::squic::{LockFreeIdTable, SQConn, id_store_optimized::UdpIdStore};

use crate::quic::QuicServer;
pub struct SunnyQuicServer {
    pub config: SunnyQuicServerCfg,
    request_sender: Sender<ProxyRequest>,
    request: Receiver<ProxyRequest>,
}

impl SunnyQuicServer {
    pub fn new(cfg: SunnyQuicServerCfg) -> Result<Self, SError> {
        let (send, recv) = channel::<ProxyRequest>(10);

        Ok(Self {
            config: cfg,
            request_sender: send,
            request: recv,
        })
    }

    async fn handle_incoming<C: QuicConnection>(
        incom: C,
        req_sender: Sender<ProxyRequest>,
        user_hash: SunnyQuicUsers,
    ) -> Result<(), SError> {
        let sq_conn = SQServerConn {
            inner: SQConn {
                conn: incom,
                authed: Arc::new(SetOnce::new()),
                auth_token: Arc::new(SetOnce::new()),
                send_id_counter: Arc::new(AtomicU16::new(1)),
                recv_id_store: Arc::new(UdpIdStore::new()),
                lock_free_id_table: Arc::new(LockFreeIdTable::new(1024)),
            },
            users: user_hash,
        };
        let span = trace_span!("quic", id = sq_conn.inner.peer_id());
        sq_conn
            .handle_connection(req_sender)
            .instrument(span)
            .await?;

        Ok(())
    }
    fn gen_users_hash(&self) -> SunnyQuicUsers {
        let users = FxHashMap::from_iter(self.config.users.iter().map(|x| {
            let hash = crate::sunnyquic::gen_sunny_user_hash(&x.username, &x.password);
            (hash, x.username.clone())
        }));
        Arc::new(users)
    }
}

#[async_trait]
impl Inbound for SunnyQuicServer {
    async fn accept(&mut self) -> Result<crate::ProxyRequest, SError> {
        let req = self
            .request
            .recv()
            .await
            .ok_or(SError::InboundUnavailable)?;
        return Ok(req);
    }
    /// Init background job for accepting connection
    async fn init(&self) -> Result<(), SError> {
        let request_sender = self.request_sender.clone();
        let config = self.config.clone();
        let user_hash = self.gen_users_hash();
        let fut = async move {
            let endpoint: EndServer = QuicServer::new(&config)
                .await
                .expect("Failed to listening on udp");
            loop {
                match QuicServer::accept(&endpoint).await {
                    Ok(conn) => {
                        let request_sender = request_sender.clone();
                        let user_hash = user_hash.clone();
                        tokio::spawn(async move {
                            Self::handle_incoming(conn, request_sender, user_hash)
                                .await
                                .map_err(|x| error!("{}", x))
                        });
                    }
                    Err(e) => {
                        error!("Error accepting quic connection: {}", e);
                    }
                }
            }
        };
        tokio::spawn(fut);
        Ok(())
    }
}
