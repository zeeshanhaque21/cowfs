use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::context::RPCContext;
use crate::reply_cache::ReplyCache;
use crate::rpcwire::serve_connection;
use crate::vfs::NFSFileSystem;

/// Resource bounds of a server. The defaults suit one local client that mounts and then talks
/// over one or two connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Connections served at once. Further connections are closed as they arrive.
    pub max_connections: usize,
    /// How long an established connection may sit without a request.
    pub idle_timeout: Duration,
    /// How long a connection may take to send its first request, and any request to arrive once
    /// its first byte has (slowloris bound).
    pub frame_timeout: Duration,
    /// Requests of one connection running at once, the reader stops reading beyond that.
    pub max_in_flight: usize,
    /// Largest RPC message accepted, in bytes: a maximal WRITE plus headers.
    pub max_frame: usize,
    /// Calls remembered for retransmission replay.
    pub reply_cache_entries: usize,
    /// Age after which a remembered call is forgotten.
    pub reply_cache_age: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 64,
            idle_timeout: Duration::from_secs(15 * 60),
            frame_timeout: Duration::from_secs(30),
            max_in_flight: 32,
            max_frame: 1024 * 1024 + 64 * 1024,
            reply_cache_entries: 4096,
            reply_cache_age: Duration::from_secs(60),
        }
    }
}

/// Lets exactly one MNT take the root file handle. The first MNT claims it and every later MNT,
/// from any connection or source address (the claimer's included), is refused until `rearm`.
/// Unmounting does not rearm it, so a process that sends UMNT cannot reopen the gate.
#[derive(Debug, Default)]
pub struct MountGate {
    claimed: AtomicBool,
    refused: AtomicBool,
}

impl MountGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// True for the first caller since creation or the last `rearm`, false for every other.
    pub fn claim(&self) -> bool {
        if self.claimed.swap(true, Ordering::AcqRel) {
            self.refused.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Allows the next MNT to claim the gate, for a deliberate remount.
    pub fn rearm(&self) {
        self.claimed.store(false, Ordering::Release);
    }

    /// True if a MNT was refused since the gate was created.
    pub fn refused_any(&self) -> bool {
        self.refused.load(Ordering::Relaxed)
    }
}

/// Decides whether the connection that sends MNT may mount: called with the (peer, local)
/// address on a blocking thread. Only MNT is checked: the macOS kernel NFS client owns its
/// sockets, so no process can be found behind them.
pub type PeerCheck = Arc<dyn Fn(SocketAddr, SocketAddr) -> bool + Send + Sync>;

/// A NFS Tcp Connection Handler
pub struct NFSTcpListener<T: NFSFileSystem + Send + Sync + 'static> {
    listener: TcpListener,
    port: u16,
    arcfs: Arc<T>,
    mount_signal: Option<mpsc::Sender<bool>>,
    mount_gate: Option<Arc<MountGate>>,
    peer_check: Option<PeerCheck>,
    export_name: Arc<String>,
    limits: Limits,
    reply_cache: Arc<ReplyCache>,
    live: Arc<Mutex<Vec<Live>>>,
    next_conn: AtomicU64,
}

/// One served connection, so the accept loop can make room by dropping the least served, oldest
/// one instead of refusing whoever arrives next. `served` counts only requests that
/// carried a valid file handle or a successful MNT, so cheap traffic (NULL, refused MNT, forged
/// handles) cannot raise a connection above the real client.
#[derive(Debug)]
struct Live {
    id: u64,
    /// When this connection last had a request, and how many it has carried, both shared with
    /// its context so every request refreshes them.
    active: Arc<AtomicU64>,
    served: Arc<AtomicU64>,
    kick: tokio::sync::watch::Sender<bool>,
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl<T: NFSFileSystem + Send + Sync + 'static> std::fmt::Debug for NFSTcpListener<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NFSTcpListener")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait NFSTcp: Send + Sync {
    /// Gets the true listening port. Useful if the bound port number is 0
    fn get_listen_port(&self) -> u16;

    /// Gets the true listening IP.
    fn get_listen_ip(&self) -> IpAddr;

    /// Sets a mount listener. A "true" signal will be sent on a mount
    /// and a "false" will be sent on an unmount
    fn set_mount_listener(&mut self, signal: mpsc::Sender<bool>);

    /// Loops forever and never returns handling all incoming connections.
    async fn handle_forever(&self) -> io::Result<()>;
}

impl<T: NFSFileSystem + Send + Sync + 'static> NFSTcpListener<T> {
    /// Binds to an address of the form `ip:port`, for instance "127.0.0.1:12000".
    /// Port 0 picks a free port, see `get_listen_port`.
    pub async fn bind(ipstr: &str, fs: T) -> io::Result<NFSTcpListener<T>> {
        let listener = TcpListener::bind(ipstr).await?;
        let port = listener.local_addr()?.port();
        info!("Listening on {:?}", ipstr);
        let limits = Limits::default();
        Ok(NFSTcpListener {
            listener,
            port,
            arcfs: Arc::new(fs),
            mount_signal: None,
            mount_gate: None,
            peer_check: None,
            export_name: Arc::from("/".to_string()),
            reply_cache: reply_cache(&limits),
            live: Arc::new(Mutex::new(Vec::new())),
            next_conn: AtomicU64::new(1),
            limits,
        })
    }

    /// Replaces the resource bounds. Call before `handle_forever`.
    pub fn set_limits(&mut self, limits: Limits) {
        self.reply_cache = reply_cache(&limits);
        self.limits = limits;
    }

    /// Makes MNT one-shot, see `MountGate`. Call before `handle_forever`.
    pub fn set_mount_gate(&mut self, gate: Arc<MountGate>) {
        self.mount_gate = Some(gate);
    }

    /// Sets the check MNT callers must pass, see `PeerCheck`. Call before `handle_forever`.
    pub fn set_peer_check(&mut self, check: PeerCheck) {
        self.peer_check = Some(check);
    }

    /// Sets an optional NFS export name.
    ///
    /// - `export_name`: The desired export name without slashes.
    ///
    /// Example: Name `foo` results in the export path `/foo`.
    /// Default path is `/` if not set.
    pub fn with_export_name<S: AsRef<str>>(&mut self, export_name: S) {
        self.export_name = Arc::new(format!(
            "/{}",
            export_name
                .as_ref()
                .trim_end_matches('/')
                .trim_start_matches('/')
        ))
    }
}

fn reply_cache(limits: &Limits) -> Arc<ReplyCache> {
    Arc::new(ReplyCache::new(
        limits.reply_cache_entries,
        limits.reply_cache_entries.saturating_mul(4096),
        limits.reply_cache_age,
    ))
}

#[async_trait]
impl<T: NFSFileSystem + Send + Sync + 'static> NFSTcp for NFSTcpListener<T> {
    /// Gets the true listening port. Useful if the bound port number is 0
    fn get_listen_port(&self) -> u16 {
        self.port
    }

    fn get_listen_ip(&self) -> IpAddr {
        self.listener
            .local_addr()
            .map(|a| a.ip())
            .unwrap_or(IpAddr::from([127, 0, 0, 1]))
    }

    /// Sets a mount listener. A "true" signal will be sent on a mount
    /// and a "false" will be sent on an unmount
    fn set_mount_listener(&mut self, signal: mpsc::Sender<bool>) {
        self.mount_signal = Some(signal);
    }

    /// Loops forever and never returns handling all incoming connections.
    async fn handle_forever(&self) -> io::Result<()> {
        loop {
            let (socket, peer) = match self.listener.accept().await {
                Ok(v) => v,
                Err(e) if is_transient_accept_error(&e) => {
                    debug!("accept failed: {:?}", e);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let _ = socket.set_nodelay(true);
            let Ok(local) = socket.local_addr() else {
                continue;
            };
            let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
            let active = Arc::new(AtomicU64::new(now_ms()));
            let served = Arc::new(AtomicU64::new(0));
            let (kick, kicked) = tokio::sync::watch::channel(false);
            self.enter(conn, active.clone(), served.clone(), kick.clone());
            let context = RPCContext {
                local_port: self.port,
                conn,
                client_addr: peer.to_string(),
                client_ip: peer.ip(),
                peer,
                auth: crate::rpc::auth_unix::default(),
                vfs: self.arcfs.clone(),
                mount_signal: self.mount_signal.clone(),
                mount_gate: self.mount_gate.clone(),
                peer_check: self.peer_check.clone(),
                local,
                export_name: self.export_name.clone(),
                reply_cache: self.reply_cache.clone(),
                active,
                served,
            };
            let limits = self.limits.clone();
            let live = self.live.clone();
            info!("Accepting connection from {}", context.client_addr);
            tokio::spawn(async move {
                let (rd, wr) = socket.into_split();
                serve_connection(rd, wr, context, limits, kicked).await;
                live.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retain(|l| l.id != conn);
            });
        }
    }
}

impl<T: NFSFileSystem + Send + Sync + 'static> NFSTcpListener<T> {
    /// Records a new connection, and if the cap is full kicks the least served one, oldest first.
    /// A local process cannot be told apart from the real client by address, so refusing the newcomer is what lets it lock the client out.
    fn enter(
        &self,
        conn: u64,
        active: Arc<AtomicU64>,
        served: Arc<AtomicU64>,
        kick: tokio::sync::watch::Sender<bool>,
    ) {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.retain(|l| l.active.load(Ordering::Relaxed) + 3_600_000 > now_ms());
        if live.len() >= self.limits.max_connections {
            if let Some(i) = live
                .iter()
                .enumerate()
                // Oldest connection first among the least served. Recency (`active`) is no tie-break:
                // a NULL flood keeps its connections fresh and the kernel's NFS socket, pinged
                // once and then quiet while the MNT connection arrives, would lose to it (#262).
                .min_by_key(|(_, l)| (l.served.load(Ordering::Relaxed), l.id))
                .map(|(i, _)| i)
            {
                let victim = live.remove(i);
                let _ = victim.kick.send(true);
                debug!("connection limit reached, kicked connection {}", victim.id);
            }
        }
        live.push(Live {
            id: conn,
            active,
            served,
            kick,
        });
    }
}

/// `EMFILE`. `ErrorKind::TooManyOpenFiles` is still unstable, so the number is spelled out; it is
/// the same on Linux and macOS, the only targets.
const EMFILE: i32 = 24;

fn is_transient_accept_error(e: &io::Error) -> bool {
    if e.raw_os_error() == Some(EMFILE) {
        // No descriptor left for the accepted socket. The flood that caused it is finite, and
        // returning here drops the listening socket, so the server stops accepting for good
        // instead of recovering once the flood stops (#57).
        // ponytail: a sustained flood with a full queue retries accept at syscall speed. Add a
        // short pause on EMFILE if that ever shows up as CPU burn.
        return true;
    }
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_admits_one_mnt_until_rearmed() {
        let g = MountGate::new();
        assert!(g.claim());
        assert!(!g.claim(), "not even the claimer may take a second handle");
        assert!(g.refused_any());
        g.rearm();
        assert!(g.claim());
        assert!(!g.claim());
    }
}
