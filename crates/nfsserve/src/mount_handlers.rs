use std::io::{Read, Write};
use std::sync::PoisonError;

use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::cast::FromPrimitive;
use tracing::debug;

use crate::context::RPCContext;
use crate::mount::*;
use crate::rpc::*;
use crate::xdr::*;

/*
From RFC 1813 Appendix I
program MOUNT_PROGRAM {
 version MOUNT_V3 {
    void      MOUNTPROC3_NULL(void)    = 0;
    mountres3 MOUNTPROC3_MNT(dirpath)  = 1;
    mountlist MOUNTPROC3_DUMP(void)    = 2;
    void      MOUNTPROC3_UMNT(dirpath) = 3;
    void      MOUNTPROC3_UMNTALL(void) = 4;
    exports   MOUNTPROC3_EXPORT(void)  = 5;
 } = 3;
} = 100005;
*/

#[derive(Copy, Clone, Debug, FromPrimitive, ToPrimitive)]
enum MountProgram {
    MOUNTPROC3_NULL = 0,
    MOUNTPROC3_MNT = 1,
    MOUNTPROC3_DUMP = 2,
    MOUNTPROC3_UMNT = 3,
    MOUNTPROC3_UMNTALL = 4,
    MOUNTPROC3_EXPORT = 5,
    INVALID,
}

pub async fn handle_mount(
    xid: u32,
    call: call_body,
    input: &mut impl Read,
    output: &mut impl Write,
    context: &RPCContext,
) -> Result<(), anyhow::Error> {
    let prog = MountProgram::from_u32(call.proc).unwrap_or(MountProgram::INVALID);

    match prog {
        MountProgram::MOUNTPROC3_NULL => mountproc3_null(xid, input, output)?,
        MountProgram::MOUNTPROC3_MNT => mountproc3_mnt(xid, input, output, context).await?,
        MountProgram::MOUNTPROC3_UMNT => mountproc3_umnt(xid, input, output, context).await?,
        MountProgram::MOUNTPROC3_UMNTALL => {
            mountproc3_umnt_all(xid, input, output, context).await?
        }
        MountProgram::MOUNTPROC3_EXPORT => mountproc3_export(xid, input, output, context)?,
        _ => {
            proc_unavail_reply_message(xid).serialize(output)?;
        }
    }
    Ok(())
}

pub fn mountproc3_null(
    xid: u32,
    _: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), anyhow::Error> {
    debug!("mountproc3_null({:?}) ", xid);
    // build an RPC reply
    let msg = make_success_reply(xid);
    debug!("\t{:?} --> {:?}", xid, msg);
    msg.serialize(output)?;
    Ok(())
}

#[derive(Clone, Debug)]
struct mountres3_ok {
    fhandle: fhandle3, // really same thing as nfs::nfs_fh3
    auth_flavors: Vec<u32>,
}
xdr_struct!(mountres3_ok, fhandle, auth_flavors);

pub async fn mountproc3_mnt(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    context: &RPCContext,
) -> Result<(), anyhow::Error> {
    let mut path = dirpath::new();
    path.deserialize(input)?;
    let utf8path = std::str::from_utf8(&path).unwrap_or_default();
    debug!("mountproc3_mnt({:?},{:?}) ", xid, utf8path);
    let export_name = context
        .export_name
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let path = if let Some(path) = utf8path.strip_prefix(export_name.as_str()) {
        let path = path
            .trim_start_matches('/')
            .trim_end_matches('/')
            .trim()
            .as_bytes();
        let mut new_path = Vec::with_capacity(path.len() + 1);
        new_path.push(b'/');
        new_path.extend_from_slice(path);
        new_path
    } else {
        // invalid export
        debug!("{:?} --> no matching export", xid);
        make_success_reply(xid).serialize(output)?;
        mountstat3::MNT3ERR_NOENT.serialize(output)?;
        return Ok(());
    };
    if let Some(check) = context.peer_check.clone() {
        let (peer, local) = (context.peer, context.local);
        let allowed = tokio::task::spawn_blocking(move || check(peer, local))
            .await
            .unwrap_or(false);
        if !allowed {
            debug!(
                "{:?} --> MNT3ERR_ACCES, the peer check refused {}",
                xid, peer
            );
            make_success_reply(xid).serialize(output)?;
            mountstat3::MNT3ERR_ACCES.serialize(output)?;
            return Ok(());
        }
    }
    if let Some(gate) = &context.mount_gate {
        if !gate.claim() {
            debug!(
                "{:?} --> MNT3ERR_ACCES, the root handle was already taken",
                xid
            );
            make_success_reply(xid).serialize(output)?;
            mountstat3::MNT3ERR_ACCES.serialize(output)?;
            return Ok(());
        }
    }
    if let Ok(fileid) = context.vfs.path_to_id(&path).await {
        let response = mountres3_ok {
            fhandle: context.vfs.id_to_fh(fileid).data,
            auth_flavors: vec![auth_flavor::AUTH_NULL as u32, auth_flavor::AUTH_UNIX as u32],
        };
        debug!("{:?} --> {:?}", xid, response);
        // Holding the root handle proves as much as presenting one, see `fh_or_fail`.
        context
            .served
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(ref chan) = context.mount_signal {
            let _ = chan.send(true).await;
        }
        make_success_reply(xid).serialize(output)?;
        mountstat3::MNT3_OK.serialize(output)?;
        response.serialize(output)?;
    } else {
        debug!("{:?} --> MNT3ERR_NOENT", xid);
        make_success_reply(xid).serialize(output)?;
        mountstat3::MNT3ERR_NOENT.serialize(output)?;
    }
    Ok(())
}

/*
  exports MOUNTPROC3_EXPORT(void) = 5;

  typedef struct groupnode *groups;

  struct groupnode {
       name     gr_name;
       groups   gr_next;
  };

  typedef struct exportnode *exports;

  struct exportnode {
       dirpath  ex_dir;
       groups   ex_groups;
       exports  ex_next;
  };

DESCRIPTION

  Procedure EXPORT returns a list of all the exported file
  systems and which clients are allowed to mount each one.
  The names in the group list are implementation-specific
  and cannot be directly interpreted by clients. These names
  can represent hosts or groups of hosts.

IMPLEMENTATION

  This procedure generally returns the contents of a list of
  shared or exported file systems. These are the file
  systems which are made available to NFS version 3 protocol
  clients.
 */

pub fn mountproc3_export(
    xid: u32,
    _: &mut impl Read,
    output: &mut impl Write,
    _: &RPCContext,
) -> Result<(), anyhow::Error> {
    debug!("mountproc3_export({:?}) ", xid);
    make_success_reply(xid).serialize(output)?;
    // An empty export list. The export path is a secret that only `mount_nfs` is told (see the
    // security model in cowfs-nfs), so a caller with no handle must not be able to read it here.
    false.serialize(output)?;
    Ok(())
}

pub async fn mountproc3_umnt(
    xid: u32,
    input: &mut impl Read,
    output: &mut impl Write,
    context: &RPCContext,
) -> Result<(), anyhow::Error> {
    let mut path = dirpath::new();
    path.deserialize(input)?;
    let utf8path = std::str::from_utf8(&path).unwrap_or_default();
    debug!("mountproc3_umnt({:?},{:?}) ", xid, utf8path);
    if let Some(ref chan) = context.mount_signal {
        let _ = chan.send(false).await;
    }
    make_success_reply(xid).serialize(output)?;
    mountstat3::MNT3_OK.serialize(output)?;
    Ok(())
}

pub async fn mountproc3_umnt_all(
    xid: u32,
    _input: &mut impl Read,
    output: &mut impl Write,
    context: &RPCContext,
) -> Result<(), anyhow::Error> {
    debug!("mountproc3_umnt_all({:?}) ", xid);
    if let Some(ref chan) = context.mount_signal {
        let _ = chan.send(false).await;
    }
    make_success_reply(xid).serialize(output)?;
    mountstat3::MNT3_OK.serialize(output)?;
    Ok(())
}
