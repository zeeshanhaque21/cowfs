use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Cursor, Read};
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;
use tracing::{debug, error, trace, warn};

use crate::context::RPCContext;
use crate::reply_cache::{Begin, CacheKey};
use crate::rpc::*;
use crate::tcp::Limits;
use crate::xdr::*;
use crate::{mount, mount_handlers, nfs, nfs_handlers, portmap, portmap_handlers};

// Information from RFC 5531
// https://datatracker.ietf.org/doc/html/rfc5531

const NFS_ACL_PROGRAM: u32 = 100227;
const NFS_ID_MAP_PROGRAM: u32 = 100270;
const NFS_METADATA_PROGRAM: u32 = 200024;

/// Procedures whose second execution would give a different answer: SETATTR, CREATE, MKDIR,
/// SYMLINK, REMOVE, RMDIR, RENAME, LINK. Their replies are cached for retransmissions.
const NON_IDEMPOTENT: [u32; 8] = [2, 8, 9, 10, 12, 13, 14, 15];

async fn handle_rpc(
    input: &mut impl Read,
    output: &mut Vec<u8>,
    mut context: RPCContext,
    fingerprint: u64,
) -> Result<bool, anyhow::Error> {
    let mut recv = rpc_msg::default();
    recv.deserialize(input)?;
    let xid = recv.xid;
    let rpc_body::CALL(call) = recv.body else {
        error!("Unexpectedly received a Reply instead of a Call");
        return Err(anyhow!("Bad RPC Call format"));
    };
    if let auth_flavor::AUTH_UNIX = call.cred.flavor {
        let mut auth = auth_unix::default();
        auth.deserialize(&mut Cursor::new(&call.cred.body))?;
        context.auth = auth;
    }
    if call.rpcvers != 2 {
        warn!("Invalid RPC version {} != 2", call.rpcvers);
        rpc_vers_mismatch(xid).serialize(output)?;
        return Ok(true);
    }

    let cache_key = (call.prog == nfs::PROGRAM
        && call.vers == nfs::VERSION
        && NON_IDEMPOTENT.contains(&call.proc))
    .then_some(CacheKey {
        client: context.client_ip,
        xid,
        fingerprint,
    });
    if let Some(key) = cache_key {
        match context.reply_cache.begin(key) {
            Begin::Replay(reply) => {
                debug!("replaying the reply to retransmitted xid {xid}");
                output.extend_from_slice(&reply);
                return Ok(true);
            }
            Begin::InProgress => {
                debug!("dropping retransmission of running xid {xid}");
                return Ok(false);
            }
            Begin::New => {}
        }
    }

    let res = if call.prog == nfs::PROGRAM {
        nfs_handlers::handle_nfs(xid, call, input, output, &context).await
    } else if call.prog == portmap::PROGRAM {
        portmap_handlers::handle_portmap(xid, call, input, output, &context)
    } else if call.prog == mount::PROGRAM {
        mount_handlers::handle_mount(xid, call, input, output, &context).await
    } else if call.prog == NFS_ACL_PROGRAM
        || call.prog == NFS_ID_MAP_PROGRAM
        || call.prog == NFS_METADATA_PROGRAM
    {
        trace!("ignoring NFS_ACL packet");
        prog_unavail_reply_message(xid)
            .serialize(output)
            .map_err(Into::into)
    } else {
        warn!(
            "Unknown RPC Program number {} != {}",
            call.prog,
            nfs::PROGRAM
        );
        prog_unavail_reply_message(xid)
            .serialize(output)
            .map_err(Into::into)
    };
    if let Some(key) = cache_key {
        let reply = res
            .is_ok()
            .then(|| output.get(4..).map(<[u8]>::to_vec))
            .flatten();
        context.reply_cache.finish(key, reply);
    }
    res.map(|()| true)
}

/// Reads one record (RFC 1057 section 10: a record is one or more fragments). The buffer grows
/// as bytes arrive, so a peer that declares a huge fragment and stalls costs nothing. Returns
/// `None` on a clean close between records. Waiting for the first byte of a record is bounded
/// by `first_wait`, the rest of each fragment by `limits.frame_timeout`.
async fn read_record(
    rd: &mut (impl AsyncRead + Unpin),
    limits: &Limits,
    first_wait: Duration,
) -> Result<Option<Vec<u8>>, anyhow::Error> {
    const CHUNK: usize = 64 * 1024;
    let mut record: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let mut header = [0_u8; 4];
        let wait = if first {
            first_wait
        } else {
            limits.frame_timeout
        };
        match timeout(wait, rd.read(&mut header[..1])).await {
            Err(_) => return Err(anyhow!("timed out waiting for a request")),
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok(0)) if first => return Ok(None),
            Ok(Ok(0)) => return Err(anyhow!("closed inside a record")),
            Ok(Ok(_)) => {}
        }
        first = false;
        let deadline = tokio::time::Instant::now() + limits.frame_timeout;
        timeout(
            deadline - tokio::time::Instant::now(),
            rd.read_exact(&mut header[1..]),
        )
        .await
        .map_err(|_| anyhow!("timed out inside a fragment header"))??;
        let fragment_header = u32::from_be_bytes(header);
        let is_last = (fragment_header & (1 << 31)) > 0;
        let length = (fragment_header & ((1 << 31) - 1)) as usize;
        if record.len() + length > limits.max_frame {
            return Err(anyhow!("RPC message too large"));
        }
        trace!("Reading fragment length:{}, last:{}", length, is_last);
        let end = record.len() + length;
        while record.len() < end {
            let start = record.len();
            record.resize(start + (end - start).min(CHUNK), 0);
            let read = timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                rd.read(&mut record[start..]),
            )
            .await
            .map_err(|_| anyhow!("timed out inside a fragment"))??;
            record.truncate(start + read);
            if read == 0 {
                return Err(anyhow!("closed inside a fragment"));
            }
        }
        if is_last {
            return Ok(Some(record));
        }
    }
}

/// Writes one reply as a single last fragment. `buf` must start with 4 spare bytes that receive
/// the fragment header, so header and body leave in one write (TCP_NODELAY is on).
pub async fn write_fragment(
    socket: &mut (impl AsyncWrite + Unpin),
    buf: &mut [u8],
) -> Result<(), anyhow::Error> {
    let body = u32::try_from(buf.len().saturating_sub(4))
        .ok()
        .filter(|n| *n < (1 << 31));
    let Some(body) = body else {
        return Err(anyhow!("reply too large"));
    };
    buf[..4].copy_from_slice(&(body | (1 << 31)).to_be_bytes());
    trace!("Writing fragment length:{}", body);
    socket.write_all(buf).await?;
    Ok(())
}

/// Serves one connection until the peer closes it, misbehaves or times out. Requests run
/// concurrently, at most `limits.max_in_flight` at a time.
pub async fn serve_connection(
    mut rd: OwnedReadHalf,
    mut wr: OwnedWriteHalf,
    context: RPCContext,
    limits: Limits,
) {
    let (tx, mut rx) = mpsc::channel::<Option<Vec<u8>>>(limits.max_in_flight.max(1));
    let mut writer = tokio::spawn(async move {
        while let Some(Some(mut msg)) = rx.recv().await {
            if let Err(e) = write_fragment(&mut wr, &mut msg).await {
                debug!("write failed: {e:?}");
                break;
            }
        }
    });
    let in_flight = Arc::new(Semaphore::new(limits.max_in_flight.max(1)));
    let mut served = false;
    loop {
        let first_wait = if served {
            limits.idle_timeout
        } else {
            limits.frame_timeout
        };
        let record = tokio::select! {
            r = read_record(&mut rd, &limits, first_wait) => match r {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    debug!("closing connection: {e:?}");
                    break;
                }
            },
            _ = &mut writer => break,
        };
        served = true;
        let Ok(permit) = in_flight.clone().acquire_owned().await else {
            break;
        };
        let (context, tx) = (context.clone(), tx.clone());
        tokio::spawn(async move {
            let mut hasher = DefaultHasher::new();
            record.hash(&mut hasher);
            let mut reply: Vec<u8> = vec![0; 4];
            match handle_rpc(
                &mut Cursor::new(&record[..]),
                &mut reply,
                context,
                hasher.finish(),
            )
            .await
            {
                Ok(true) => {
                    let _ = tx.send(Some(reply)).await;
                }
                Ok(false) => {}
                Err(e) => {
                    error!("RPC Error: {e:?}");
                    let _ = tx.send(None).await;
                }
            }
            drop(permit);
        });
    }
    writer.abort();
}
