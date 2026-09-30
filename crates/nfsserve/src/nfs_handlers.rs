//! NFSv3 procedure handlers (RFC 1813). Each handler decodes its arguments, calls the
//! `NFSFileSystem` and encodes the reply. A malformed call gets a GARBAGE_ARGS reply.
use std::future::Future;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::cast::FromPrimitive;
use tracing::{debug, warn};

use crate::context::RPCContext;
use crate::nfs::{self, *};
use crate::rpc::*;
use crate::vfs::VFSCapabilities;
use crate::xdr::*;

#[derive(Copy, Clone, Debug, FromPrimitive, ToPrimitive)]
enum NFSProgram {
    NFSPROC3_NULL = 0,
    NFSPROC3_GETATTR = 1,
    NFSPROC3_SETATTR = 2,
    NFSPROC3_LOOKUP = 3,
    NFSPROC3_ACCESS = 4,
    NFSPROC3_READLINK = 5,
    NFSPROC3_READ = 6,
    NFSPROC3_WRITE = 7,
    NFSPROC3_CREATE = 8,
    NFSPROC3_MKDIR = 9,
    NFSPROC3_SYMLINK = 10,
    NFSPROC3_MKNOD = 11,
    NFSPROC3_REMOVE = 12,
    NFSPROC3_RMDIR = 13,
    NFSPROC3_RENAME = 14,
    NFSPROC3_LINK = 15,
    NFSPROC3_READDIR = 16,
    NFSPROC3_READDIRPLUS = 17,
    NFSPROC3_FSSTAT = 18,
    NFSPROC3_FSINFO = 19,
    NFSPROC3_PATHCONF = 20,
    NFSPROC3_COMMIT = 21,
    INVALID = 22,
}

type Handled = Result<(), anyhow::Error>;

pub async fn handle_nfs(
    xid: u32,
    call: call_body,
    input: &mut impl Read,
    output: &mut impl Write,
    context: &RPCContext,
) -> Handled {
    if call.vers != nfs::VERSION {
        warn!(
            "Invalid NFS Version number {} != {}",
            call.vers,
            nfs::VERSION
        );
        prog_mismatch_reply_message(xid, nfs::VERSION).serialize(output)?;
        return Ok(());
    }
    let prog = NFSProgram::from_u32(call.proc).unwrap_or(NFSProgram::INVALID);
    let slot = (call.proc as usize).min(STAT_N - 1);
    let depth = INFLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
    INFLIGHT_SUM.fetch_add(depth, Ordering::Relaxed);
    let t0 = std::time::Instant::now();
    let r = handle_nfs_inner(xid, prog, input, output, context).await;
    INFLIGHT.fetch_sub(1, Ordering::Relaxed);
    STAT_COUNT[slot].fetch_add(1, Ordering::Relaxed);
    STAT_NS[slot].fetch_add(
        u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    r
}

const STAT_N: usize = 23;
static STAT_COUNT: [AtomicU64; STAT_N] = [const { AtomicU64::new(0) }; STAT_N];
static STAT_NS: [AtomicU64; STAT_N] = [const { AtomicU64::new(0) }; STAT_N];
static INFLIGHT: AtomicU64 = AtomicU64::new(0);
static INFLIGHT_SUM: AtomicU64 = AtomicU64::new(0);

/// Per-procedure op count and cumulative server-side latency, then resets.
pub fn take_stats() -> String {
    let mut s = String::from("proc count total_ms avg_us\n");
    let mut total = 0;
    for i in 0..STAT_N {
        let c = STAT_COUNT[i].swap(0, Ordering::Relaxed);
        let ns = STAT_NS[i].swap(0, Ordering::Relaxed);
        total += c;
        if c > 0 {
            let name = NFSProgram::from_usize(i)
                .map(|p| format!("{p:?}"))
                .unwrap_or(i.to_string());
            s += &format!(
                "{} {} {:.1} {:.1}\n",
                name,
                c,
                ns as f64 / 1e6,
                ns as f64 / 1e3 / c as f64
            );
        }
    }
    let sum = INFLIGHT_SUM.swap(0, Ordering::Relaxed);
    let mean = if total > 0 {
        sum as f64 / total as f64
    } else {
        0.0
    };
    s += &format!("TOTAL {total} mean_inflight_at_arrival {mean:.2}\n");
    s
}

async fn handle_nfs_inner(
    xid: u32,
    prog: NFSProgram,
    input: &mut impl Read,
    output: &mut impl Write,
    context: &RPCContext,
) -> Handled {
    use NFSProgram::*;
    match prog {
        NFSPROC3_NULL => make_success_reply(xid).serialize(output)?,
        NFSPROC3_GETATTR => nfsproc3_getattr(xid, input, output, context).await?,
        NFSPROC3_SETATTR => nfsproc3_setattr(xid, input, output, context).await?,
        NFSPROC3_LOOKUP => nfsproc3_lookup(xid, input, output, context).await?,
        NFSPROC3_ACCESS => nfsproc3_access(xid, input, output, context).await?,
        NFSPROC3_READLINK => nfsproc3_readlink(xid, input, output, context).await?,
        NFSPROC3_READ => nfsproc3_read(xid, input, output, context).await?,
        NFSPROC3_WRITE => nfsproc3_write(xid, input, output, context).await?,
        NFSPROC3_CREATE => nfsproc3_create(xid, input, output, context).await?,
        NFSPROC3_MKDIR => nfsproc3_mkdir(xid, input, output, context).await?,
        NFSPROC3_SYMLINK => nfsproc3_symlink(xid, input, output, context).await?,
        NFSPROC3_MKNOD => {
            begin(xid, output, nfsstat3::NFS3ERR_NOTSUPP)?;
            wcc_data::default().serialize(output)?
        }
        NFSPROC3_REMOVE => nfsproc3_remove(xid, input, output, context, false).await?,
        NFSPROC3_RMDIR => nfsproc3_remove(xid, input, output, context, true).await?,
        NFSPROC3_RENAME => nfsproc3_rename(xid, input, output, context).await?,
        NFSPROC3_LINK => nfsproc3_link(xid, input, output, context).await?,
        NFSPROC3_READDIR => nfsproc3_readdir(xid, input, output, context, false).await?,
        NFSPROC3_READDIRPLUS => nfsproc3_readdir(xid, input, output, context, true).await?,
        NFSPROC3_FSSTAT => nfsproc3_fsstat(xid, input, output, context).await?,
        NFSPROC3_FSINFO => nfsproc3_fsinfo(xid, input, output, context).await?,
        NFSPROC3_PATHCONF => nfsproc3_pathconf(xid, input, output, context).await?,
        NFSPROC3_COMMIT => nfsproc3_commit(xid, input, output, context).await?,
        INVALID => {
            warn!("Unimplemented message {:?}", prog);
            proc_unavail_reply_message(xid).serialize(output)?;
        }
    }
    Ok(())
}

fn begin(xid: u32, output: &mut impl Write, stat: nfsstat3) -> std::io::Result<()> {
    make_success_reply(xid).serialize(output)?;
    stat.serialize(output)
}

macro_rules! args {
    ($ty:ty, $xid:expr, $input:expr, $output:expr) => {{
        let mut a = <$ty>::default();
        if a.deserialize($input).is_err() {
            garbage_args_reply_message($xid).serialize($output)?;
            return Ok(());
        }
        a
    }};
}

macro_rules! fh_or_fail {
    ($ctx:expr, $fh:expr, $xid:expr, $output:expr, $tail:expr) => {
        match $ctx.vfs.fh_to_id($fh) {
            Ok(id) => id,
            Err(stat) => {
                begin($xid, $output, stat)?;
                $tail.serialize($output)?;
                return Ok(());
            }
        }
    };
}

macro_rules! rofs_or_fail {
    ($ctx:expr, $xid:expr, $output:expr, $tail:expr) => {
        if !matches!($ctx.vfs.capabilities(), VFSCapabilities::ReadWrite) {
            begin($xid, $output, nfsstat3::NFS3ERR_ROFS)?;
            $tail.serialize($output)?;
            return Ok(());
        }
    };
}

async fn post_attr(ctx: &RPCContext, id: fileid3) -> post_op_attr {
    match ctx.vfs.getattr(id).await {
        Ok(v) => post_op_attr::attributes(v),
        Err(_) => post_op_attr::Void,
    }
}

fn pre_of(a: &fattr3) -> pre_op_attr {
    pre_op_attr::attributes(wcc_attr {
        size: a.size,
        mtime: a.mtime,
        ctime: a.ctime,
    })
}

async fn pre_attr(ctx: &RPCContext, id: fileid3) -> pre_op_attr {
    match ctx.vfs.getattr(id).await {
        Ok(v) => pre_of(&v),
        Err(_) => pre_op_attr::Void,
    }
}

/// Runs `op` between a before and an after `getattr` of `id`.
async fn with_wcc<T>(ctx: &RPCContext, id: fileid3, op: impl Future<Output = T>) -> (wcc_data, T) {
    let before = pre_attr(ctx, id).await;
    let out = op.await;
    let after = post_attr(ctx, id).await;
    (wcc_data { before, after }, out)
}

pub async fn nfsproc3_getattr(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let handle = args!(nfs_fh3, xid, input, output);
    let id = fh_or_fail!(ctx, &handle, xid, output, ());
    match ctx.vfs.getattr(id).await {
        Ok(a) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            a.serialize(output)?;
        }
        Err(stat) => begin(xid, output, stat)?,
    }
    Ok(())
}

pub async fn nfsproc3_lookup(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let dirops = args!(diropargs3, xid, input, output);
    let dirid = fh_or_fail!(ctx, &dirops.dir, xid, output, post_op_attr::Void);
    let (found, dir_attr) =
        tokio::join!(ctx.vfs.lookup(dirid, &dirops.name), post_attr(ctx, dirid));
    match found {
        Ok((fid, attr)) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            ctx.vfs.id_to_fh(fid).serialize(output)?;
            post_op_attr::attributes(attr).serialize(output)?;
            dir_attr.serialize(output)?;
        }
        Err(stat) => {
            debug!("lookup {:?} in {} --> {:?}", dirops.name, dirid, stat);
            begin(xid, output, stat)?;
            dir_attr.serialize(output)?;
        }
    }
    Ok(())
}

const ACCESS3_READ: u32 = 0x0001;
const ACCESS3_LOOKUP: u32 = 0x0002;
const ACCESS3_MODIFY: u32 = 0x0004;
const ACCESS3_EXTEND: u32 = 0x0008;
const ACCESS3_DELETE: u32 = 0x0010;
const ACCESS3_EXECUTE: u32 = 0x0020;

/// The ACCESS bits granted to the owner (everything is owned by the mounter) for `requested`.
pub fn access_granted(attr: &fattr3, requested: u32, writable: bool) -> u32 {
    let r = attr.mode & 0o400 != 0;
    let w = attr.mode & 0o200 != 0 && writable;
    let x = attr.mode & 0o100 != 0;
    let mut granted = 0;
    if r {
        granted |= ACCESS3_READ;
    }
    if w {
        granted |= ACCESS3_MODIFY | ACCESS3_EXTEND;
    }
    if matches!(attr.ftype, ftype3::NF3DIR) {
        if x {
            granted |= ACCESS3_LOOKUP;
        }
        if w {
            granted |= ACCESS3_DELETE;
        }
    } else if x {
        granted |= ACCESS3_EXECUTE;
    }
    requested & granted
}

pub async fn nfsproc3_access(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let mut a = (nfs_fh3::default(), 0u32);
    if a.deserialize(input).is_err() {
        garbage_args_reply_message(xid).serialize(output)?;
        return Ok(());
    }
    let id = fh_or_fail!(ctx, &a.0, xid, output, post_op_attr::Void);
    match ctx.vfs.getattr(id).await {
        Ok(attr) => {
            let writable = matches!(ctx.vfs.capabilities(), VFSCapabilities::ReadWrite);
            begin(xid, output, nfsstat3::NFS3_OK)?;
            post_op_attr::attributes(attr).serialize(output)?;
            access_granted(&attr, a.1, writable).serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            post_op_attr::Void.serialize(output)?;
        }
    }
    Ok(())
}

pub async fn nfsproc3_readlink(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let handle = args!(nfs_fh3, xid, input, output);
    let id = fh_or_fail!(ctx, &handle, xid, output, post_op_attr::Void);
    let (target, attr) = tokio::join!(ctx.vfs.readlink(id), post_attr(ctx, id));
    match target {
        Ok(path) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            attr.serialize(output)?;
            path.serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            attr.serialize(output)?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct READ3args {
    file: nfs_fh3,
    offset: offset3,
    count: count3,
}
xdr_struct!(READ3args, file, offset, count);

const MAX_TRANSFER: u32 = 1024 * 1024;

pub async fn nfsproc3_read(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let args = args!(READ3args, xid, input, output);
    let id = fh_or_fail!(ctx, &args.file, xid, output, post_op_attr::Void);
    let (data, attr) = tokio::join!(
        ctx.vfs.read(id, args.offset, args.count.min(MAX_TRANSFER)),
        post_attr(ctx, id)
    );
    match data {
        Ok((bytes, eof)) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            attr.serialize(output)?;
            (u32::try_from(bytes.len()).unwrap_or(u32::MAX), (eof, bytes)).serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            attr.serialize(output)?;
        }
    }
    Ok(())
}

#[derive(Copy, Clone, Debug, Default, FromPrimitive, ToPrimitive)]
#[repr(u32)]
pub enum stable_how {
    #[default]
    UNSTABLE = 0,
    DATA_SYNC = 1,
    FILE_SYNC = 2,
}
xdr_enum_serde!(stable_how);

#[derive(Debug, Default)]
struct WRITE3args {
    file: nfs_fh3,
    offset: offset3,
    count: count3,
    stable: u32,
    data: Vec<u8>,
}
xdr_struct!(WRITE3args, file, offset, count, stable, data);

pub async fn nfsproc3_write(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    rofs_or_fail!(ctx, xid, output, wcc_data::default());
    let args = args!(WRITE3args, xid, input, output);
    if args.data.len() != args.count as usize || args.count > MAX_TRANSFER {
        garbage_args_reply_message(xid).serialize(output)?;
        return Ok(());
    }
    let id = fh_or_fail!(ctx, &args.file, xid, output, wcc_data::default());
    let stable = stable_how::from_u32(args.stable).unwrap_or(stable_how::FILE_SYNC);
    let (wcc, res) = with_wcc(ctx, id, async {
        let (n, _) = ctx.vfs.write(id, args.offset, args.data).await?;
        if !matches!(stable, stable_how::UNSTABLE) {
            ctx.vfs.commit(id).await?;
        }
        Ok::<u32, nfsstat3>(n)
    })
    .await;
    match res {
        Ok(count) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            wcc.serialize(output)?;
            count.serialize(output)?;
            stable.serialize(output)?;
            ctx.vfs.serverid().serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            wcc.serialize(output)?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct COMMIT3args {
    file: nfs_fh3,
    offset: offset3,
    count: count3,
}
xdr_struct!(COMMIT3args, file, offset, count);

pub async fn nfsproc3_commit(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let args = args!(COMMIT3args, xid, input, output);
    let id = fh_or_fail!(ctx, &args.file, xid, output, wcc_data::default());
    let (wcc, res) = with_wcc(ctx, id, ctx.vfs.commit(id)).await;
    match res {
        Ok(()) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            wcc.serialize(output)?;
            ctx.vfs.serverid().serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            wcc.serialize(output)?;
        }
    }
    Ok(())
}

#[derive(Copy, Clone, Debug, Default, FromPrimitive, ToPrimitive)]
#[repr(u32)]
pub enum createmode3 {
    #[default]
    UNCHECKED = 0,
    GUARDED = 1,
    EXCLUSIVE = 2,
}
xdr_enum_serde!(createmode3);

enum CreateHow {
    Attr(sattr3, bool),
    Exclusive(createverf3),
}

fn read_create_args(input: &mut impl Read) -> std::io::Result<(diropargs3, CreateHow)> {
    let mut dirops = diropargs3::default();
    dirops.deserialize(input)?;
    let mut mode = createmode3::default();
    mode.deserialize(input)?;
    let how = if let createmode3::EXCLUSIVE = mode {
        let mut verf = createverf3::default();
        verf.deserialize(input)?;
        CreateHow::Exclusive(verf)
    } else {
        let mut attr = sattr3::default();
        attr.deserialize(input)?;
        CreateHow::Attr(attr, matches!(mode, createmode3::GUARDED))
    };
    Ok((dirops, how))
}

/// Replies to CREATE, MKDIR and SYMLINK, which share one reply shape.
fn created_reply(
    xid: u32,
    output: &mut impl Write,
    ctx: &RPCContext,
    wcc: wcc_data,
    res: Result<(fileid3, fattr3), nfsstat3>,
) -> Handled {
    match res {
        Ok((fid, attr)) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            post_op_fh3::handle(ctx.vfs.id_to_fh(fid)).serialize(output)?;
            post_op_attr::attributes(attr).serialize(output)?;
        }
        Err(stat) => begin(xid, output, stat)?,
    }
    wcc.serialize(output)?;
    Ok(())
}

pub async fn nfsproc3_create(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    rofs_or_fail!(ctx, xid, output, wcc_data::default());
    let Ok((dirops, how)) = read_create_args(input) else {
        garbage_args_reply_message(xid).serialize(output)?;
        return Ok(());
    };
    let dirid = fh_or_fail!(ctx, &dirops.dir, xid, output, wcc_data::default());
    let (wcc, res) = with_wcc(ctx, dirid, async {
        match how {
            CreateHow::Attr(attr, guarded) => {
                ctx.vfs.create(dirid, &dirops.name, attr, guarded).await
            }
            CreateHow::Exclusive(verf) => ctx.vfs.create_exclusive(dirid, &dirops.name, verf).await,
        }
    })
    .await;
    created_reply(xid, output, ctx, wcc, res)
}

#[derive(Clone, Debug, Default)]
pub enum sattrguard3 {
    #[default]
    Void,
    obj_ctime(nfstime3),
}
xdr_bool_union!(sattrguard3, obj_ctime, nfstime3);

#[derive(Clone, Debug, Default)]
struct SETATTR3args {
    object: nfs_fh3,
    new_attribute: sattr3,
    guard: sattrguard3,
}
xdr_struct!(SETATTR3args, object, new_attribute, guard);

pub async fn nfsproc3_setattr(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    rofs_or_fail!(ctx, xid, output, wcc_data::default());
    let args = args!(SETATTR3args, xid, input, output);
    let id = fh_or_fail!(ctx, &args.object, xid, output, wcc_data::default());
    let (wcc, res) = with_wcc(ctx, id, async {
        if let sattrguard3::obj_ctime(c) = args.guard {
            let cur = ctx.vfs.getattr(id).await?;
            if c.seconds != cur.ctime.seconds || c.nseconds != cur.ctime.nseconds {
                return Err(nfsstat3::NFS3ERR_NOT_SYNC);
            }
        }
        ctx.vfs.setattr(id, args.new_attribute).await
    })
    .await;
    begin(xid, output, res.err().unwrap_or(nfsstat3::NFS3_OK))?;
    wcc.serialize(output)?;
    Ok(())
}

pub async fn nfsproc3_remove(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
    is_rmdir: bool,
) -> Handled {
    rofs_or_fail!(ctx, xid, output, wcc_data::default());
    let dirops = args!(diropargs3, xid, input, output);
    let dirid = fh_or_fail!(ctx, &dirops.dir, xid, output, wcc_data::default());
    let (wcc, res) = with_wcc(ctx, dirid, async {
        if is_rmdir {
            ctx.vfs.rmdir(dirid, &dirops.name).await
        } else {
            ctx.vfs.remove(dirid, &dirops.name).await
        }
    })
    .await;
    begin(xid, output, res.err().unwrap_or(nfsstat3::NFS3_OK))?;
    wcc.serialize(output)?;
    Ok(())
}

pub async fn nfsproc3_rename(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let none = (wcc_data::default(), wcc_data::default());
    rofs_or_fail!(ctx, xid, output, none);
    let mut a = (diropargs3::default(), diropargs3::default());
    if a.deserialize(input).is_err() {
        garbage_args_reply_message(xid).serialize(output)?;
        return Ok(());
    }
    let (from, to) = a;
    let from_dir = fh_or_fail!(ctx, &from.dir, xid, output, none);
    let to_dir = fh_or_fail!(ctx, &to.dir, xid, output, none);
    let (from_wcc, (to_wcc, res)) = with_wcc(
        ctx,
        from_dir,
        with_wcc(
            ctx,
            to_dir,
            ctx.vfs.rename(from_dir, &from.name, to_dir, &to.name),
        ),
    )
    .await;
    begin(xid, output, res.err().unwrap_or(nfsstat3::NFS3_OK))?;
    (from_wcc, to_wcc).serialize(output)?;
    Ok(())
}

#[derive(Debug, Default)]
struct MKDIR3args {
    dirops: diropargs3,
    attributes: sattr3,
}
xdr_struct!(MKDIR3args, dirops, attributes);

pub async fn nfsproc3_mkdir(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    rofs_or_fail!(ctx, xid, output, wcc_data::default());
    let args = args!(MKDIR3args, xid, input, output);
    let dirid = fh_or_fail!(ctx, &args.dirops.dir, xid, output, wcc_data::default());
    let (wcc, res) = with_wcc(
        ctx,
        dirid,
        ctx.vfs.mkdir(dirid, &args.dirops.name, &args.attributes),
    )
    .await;
    created_reply(xid, output, ctx, wcc, res)
}

#[derive(Debug, Default)]
struct SYMLINK3args {
    dirops: diropargs3,
    symlink: symlinkdata3,
}
xdr_struct!(SYMLINK3args, dirops, symlink);

pub async fn nfsproc3_symlink(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    rofs_or_fail!(ctx, xid, output, wcc_data::default());
    let args = args!(SYMLINK3args, xid, input, output);
    let dirid = fh_or_fail!(ctx, &args.dirops.dir, xid, output, wcc_data::default());
    let (wcc, res) = with_wcc(
        ctx,
        dirid,
        ctx.vfs.symlink(
            dirid,
            &args.dirops.name,
            &args.symlink.symlink_data,
            &args.symlink.symlink_attributes,
        ),
    )
    .await;
    created_reply(xid, output, ctx, wcc, res)
}

#[derive(Debug, Default)]
struct LINK3args {
    file: nfs_fh3,
    link: diropargs3,
}
xdr_struct!(LINK3args, file, link);

pub async fn nfsproc3_link(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let none = (post_op_attr::Void, wcc_data::default());
    rofs_or_fail!(ctx, xid, output, none);
    let args = args!(LINK3args, xid, input, output);
    let fileid = fh_or_fail!(ctx, &args.file, xid, output, none);
    let dirid = fh_or_fail!(ctx, &args.link.dir, xid, output, none);
    let (wcc, res) = with_wcc(ctx, dirid, ctx.vfs.link(fileid, dirid, &args.link.name)).await;
    let file_attr = match res {
        Ok(a) => post_op_attr::attributes(a),
        Err(_) => post_attr(ctx, fileid).await,
    };
    begin(xid, output, res.err().unwrap_or(nfsstat3::NFS3_OK))?;
    file_attr.serialize(output)?;
    wcc.serialize(output)?;
    Ok(())
}

#[derive(Debug, Default)]
struct READDIR3args {
    dir: nfs_fh3,
    cookie: cookie3,
    cookieverf: cookieverf3,
    dircount: count3,
    maxcount: count3,
}

impl READDIR3args {
    fn read(input: &mut impl Read, plus: bool) -> std::io::Result<Self> {
        let mut a = Self::default();
        a.dir.deserialize(input)?;
        a.cookie.deserialize(input)?;
        a.cookieverf.deserialize(input)?;
        a.dircount.deserialize(input)?;
        if plus {
            a.maxcount.deserialize(input)?;
        } else {
            a.maxcount = a.dircount;
        }
        Ok(a)
    }
}

/// Largest directory reply built, whatever the client asks for.
const MAX_DIR_REPLY: u32 = 256 * 1024;
const MAX_DIR_ENTRIES: u32 = 4096;
const DIRENT_MIN_BYTES: u32 = 24;
const DIRENTPLUS_MIN_BYTES: u32 = 140;

/// READDIR and READDIRPLUS. Entries are packed until `dircount` (name bytes) or `maxcount`
/// (reply bytes) runs out. The cookie is the resume point of the last entry sent.
pub async fn nfsproc3_readdir(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
    plus: bool,
) -> Handled {
    let Ok(mut args) = READDIR3args::read(input, plus) else {
        garbage_args_reply_message(xid).serialize(output)?;
        return Ok(());
    };
    args.dircount = args.dircount.min(MAX_DIR_REPLY);
    args.maxcount = args.maxcount.min(MAX_DIR_REPLY);
    let dirid = fh_or_fail!(ctx, &args.dir, xid, output, post_op_attr::Void);
    let mut want = (args.dircount / DIRENT_MIN_BYTES).min(MAX_DIR_ENTRIES);
    if plus {
        want = want.min(args.maxcount / DIRENTPLUS_MIN_BYTES);
    }
    let (listing, dir_attr) = tokio::join!(
        ctx.vfs
            .readdir(dirid, args.cookie, want.max(1) as usize, plus),
        post_attr(ctx, dirid)
    );
    let result = match listing {
        Ok(r) => r,
        Err(stat) => {
            begin(xid, output, stat)?;
            dir_attr.serialize(output)?;
            return Ok(());
        }
    };

    // RPC header, status, directory attributes, verifier, list terminator and eof
    let budget = (args.maxcount as usize).saturating_sub(160);
    let max_dircount = args.dircount as usize;
    let mut body: Vec<u8> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut dircount = 0usize;
    let mut all_written = true;
    let mut written = 0usize;
    for entry in &result.entries {
        scratch.clear();
        true.serialize(&mut scratch)?;
        entry.fileid.serialize(&mut scratch)?;
        entry.name.serialize(&mut scratch)?;
        entry.cookie.serialize(&mut scratch)?;
        if plus {
            entry
                .attr
                .map_or(post_op_attr::Void, post_op_attr::attributes)
                .serialize(&mut scratch)?;
            post_op_fh3::handle(ctx.vfs.id_to_fh(entry.fileid)).serialize(&mut scratch)?;
        }
        let added_dircount = 8 + 4 + entry.name.len() + 8;
        if body.len() + scratch.len() > budget || dircount + added_dircount > max_dircount {
            all_written = false;
            break;
        }
        body.extend_from_slice(&scratch);
        dircount += added_dircount;
        written += 1;
    }
    if written == 0 && !result.entries.is_empty() {
        begin(xid, output, nfsstat3::NFS3ERR_TOOSMALL)?;
        dir_attr.serialize(output)?;
        return Ok(());
    }
    begin(xid, output, nfsstat3::NFS3_OK)?;
    dir_attr.serialize(output)?;
    cookieverf3::default().serialize(output)?;
    output.write_all(&body)?;
    false.serialize(output)?;
    (all_written && result.end).serialize(output)?;
    Ok(())
}

pub async fn nfsproc3_fsstat(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let handle = args!(nfs_fh3, xid, input, output);
    let id = fh_or_fail!(ctx, &handle, xid, output, post_op_attr::Void);
    let (stat, attr) = tokio::join!(ctx.vfs.fsstat(id), post_attr(ctx, id));
    match stat {
        Ok(mut res) => {
            res.obj_attributes = attr;
            begin(xid, output, nfsstat3::NFS3_OK)?;
            res.serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            attr.serialize(output)?;
        }
    }
    Ok(())
}

pub async fn nfsproc3_fsinfo(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let handle = args!(nfs_fh3, xid, input, output);
    let id = fh_or_fail!(ctx, &handle, xid, output, post_op_attr::Void);
    match ctx.vfs.fsinfo(id).await {
        Ok(res) => {
            begin(xid, output, nfsstat3::NFS3_OK)?;
            res.serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            post_op_attr::Void.serialize(output)?;
        }
    }
    Ok(())
}

pub async fn nfsproc3_pathconf(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    ctx: &RPCContext,
) -> Handled {
    let handle = args!(nfs_fh3, xid, input, output);
    let id = fh_or_fail!(ctx, &handle, xid, output, post_op_attr::Void);
    let (conf, attr) = tokio::join!(ctx.vfs.pathconf(id), post_attr(ctx, id));
    match conf {
        Ok(mut res) => {
            res.obj_attributes = attr;
            begin(xid, output, nfsstat3::NFS3_OK)?;
            res.serialize(output)?;
        }
        Err(stat) => {
            begin(xid, output, stat)?;
            attr.serialize(output)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(mode: u32, ftype: ftype3) -> fattr3 {
        fattr3 {
            ftype,
            mode,
            ..fattr3::default()
        }
    }

    #[test]
    fn access_follows_the_owner_bits() {
        let all = 0x3f;
        let file = attr(0o644, ftype3::NF3REG);
        assert_eq!(
            access_granted(&file, all, true),
            ACCESS3_READ | ACCESS3_MODIFY | ACCESS3_EXTEND
        );
        assert_eq!(access_granted(&file, all, false), ACCESS3_READ);
        let exe = attr(0o755, ftype3::NF3REG);
        assert_ne!(access_granted(&exe, all, true) & ACCESS3_EXECUTE, 0);
        let dir = attr(0o755, ftype3::NF3DIR);
        let g = access_granted(&dir, all, true);
        assert_ne!(g & ACCESS3_LOOKUP, 0);
        assert_ne!(g & ACCESS3_DELETE, 0);
        assert_eq!(g & ACCESS3_EXECUTE, 0);
        assert_eq!(
            access_granted(&attr(0o444, ftype3::NF3DIR), all, true) & ACCESS3_DELETE,
            0
        );
        assert_eq!(
            access_granted(&file, ACCESS3_READ, true),
            ACCESS3_READ,
            "only what was asked"
        );
    }

    #[test]
    fn stats_table_counts_and_resets() {
        STAT_COUNT[1].fetch_add(2, Ordering::Relaxed);
        STAT_NS[1].fetch_add(4000, Ordering::Relaxed);
        let s = take_stats();
        assert!(s.starts_with("proc count total_ms avg_us\n"));
        assert!(s.contains("NFSPROC3_GETATTR"), "{s}");
        assert!(s.contains("TOTAL"));
    }
}
