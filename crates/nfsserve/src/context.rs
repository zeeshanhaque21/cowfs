use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::reply_cache::ReplyCache;
use crate::tcp::{MountGate, PeerCheck};
use crate::vfs::NFSFileSystem;

#[derive(Clone)]
pub struct RPCContext {
    pub local_port: u16,
    /// Identity of the connection this request arrived on, see `ReplyCache`.
    pub conn: u64,
    /// When this connection last had a request, so the accept loop can kick the quiet one.
    pub active: Arc<std::sync::atomic::AtomicU64>,
    /// How many requests with a valid file handle (or a successful MNT) this connection has
    /// carried, so the accept loop kicks the ones that have carried none (NULL calls, refused
    /// MNT, bad handles) first.
    pub served: Arc<std::sync::atomic::AtomicU64>,
    pub client_addr: String,
    pub client_ip: IpAddr,
    pub peer: SocketAddr,
    pub auth: crate::rpc::auth_unix,
    pub vfs: Arc<dyn NFSFileSystem + Send + Sync>,
    pub mount_signal: Option<mpsc::Sender<bool>>,
    pub mount_gate: Option<Arc<MountGate>>,
    pub peer_check: Option<PeerCheck>,
    pub local: SocketAddr,
    pub export_name: Arc<Mutex<String>>,
    pub reply_cache: Arc<ReplyCache>,
    /// See `Limits::handler_timeout`.
    pub handler_timeout: std::time::Duration,
}

impl fmt::Debug for RPCContext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RPCContext")
            .field("local_port", &self.local_port)
            .field("client_addr", &self.client_addr)
            .field("auth", &self.auth)
            .finish()
    }
}
