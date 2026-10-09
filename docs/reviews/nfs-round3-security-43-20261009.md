# NFS adapter round 3 security review (#43)

Reviewed origin/main at 84bd305, crates/cowfs-nfs and crates/nfsserve, read-only.
All findings are reasoned from source.
No PoC tests were run in this round, so nothing below is verified by test.
Items 2 (AppleDouble Translate), 3 (dead-server hang wiring) and 4 (Store mode `._` files) were not reviewed in this round and need a follow-up pass.

## Findings

### F1 BLOCK (reasoned): MOUNT EXPORT discloses the secret export path

File: crates/nfsserve/src/mount_handlers.rs:188-204 (`mountproc3_export`).
The EXPORT procedure returns `context.export_name` to any connection, with no handle and no gate check.
The security model in crates/cowfs-nfs/src/lib.rs:74-79 depends on the export path staying secret, so this removes the first line of defence.
What remains is the one-shot MNT gate, so this turns into a race on the first MNT (see F2).
Fix: answer EXPORT with an empty list (or PROC_UNAVAIL).
Add a protocol test asserting that EXPORT does not contain the export name.

### F2 BLOCK (reasoned): the one-shot gate is keyed by peer SocketAddr and has a first-MNT race

File: crates/nfsserve/src/tcp.rs:55-91 (`MountGate::claim`), crates/cowfs-nfs/src/mount.rs:154-204 and 340-381.
The gate grants the root handle to the first MNT it sees and then re-admits any later MNT from the same `SocketAddr`.
The server is listening before `mount_nfs` is spawned, so a local process that learns the path (F1) can try to MNT first.
Once the claimed source address is free again, a new connection from that address also passes the gate.
Fix: keep the gate claimed after the first successful MNT (claim once, never compare addresses), and treat any refused MNT (`refused_any`) as a reason to tear down and fail the mount.
Also consider handing the secret to `mount_nfs` some way other than argv, since argv is visible to other processes while it runs.

### F3 NOTE, possibly BLOCK as local DoS (reasoned): eviction at the connection cap favours high-traffic connections

File: crates/nfsserve/src/tcp.rs:290-326 (`enter`), crates/nfsserve/src/rpcwire.rs:37-42.
At `max_connections` (default 64) the victim is the live connection with the fewest served requests.
`served` counts every record, including NULL calls that need no handle.
So the choice favours whoever sends the most cheap traffic, not whoever is the real client.
A recently reconnected kernel client has a low count and is the likely victim, and on a `hard` mount that means hung callers.
Fix: exempt connections that have presented a valid file handle from eviction, or count only handle-bearing requests toward `served`.
Evict unauthenticated connections first.

### F4 NOTE (reasoned): cross-connection replay detection misfires under pipelining

File: crates/nfsserve/src/reply_cache.rs:88-103 and 127-134.
`moved_on` treats "the original connection sent a higher xid" as "the client got the reply".
With two concurrent non-idempotent calls (say xid 7 and 8), a reconnect retransmission of 7 is executed again instead of replayed.
That is the case the cross index exists for (for example REMOVE returning NOENT during `rm -rf`).
xid wrap fails safe (it re-executes), but has the same effect.
Fix: record which xids actually had a reply written on the original connection, rather than relying on a high-water mark.

### F5 NOTE (reasoned): reply cache can be churned by any connection

File: crates/nfsserve/src/rpcwire.rs:61-116, reply_cache.rs:169-193.
Non-idempotent calls with bad handles still produce cached replies, so any connection can fill the 4096 entries and push out the legitimate client's entries.
Fix: do not cache replies to calls whose handle failed to decode.

### F6 NOTE (reasoned): handle codec observations

File: crates/cowfs-nfs/src/handle.rs.
The key comes from /dev/urandom, the MAC is a 16-byte keyed BLAKE3 over generation, ino, inode generation and kind, and the comparison is constant time.
Handles from a previous server are refused, as STALE by generation or as BADHANDLE by key.
The MAC does not name the export, but each server has its own key, so handles do not carry across exports.
Once more than `MAX_REMEMBERED` (2^18) inodes have been buried, the oldest generations are forgotten.
That only matters for a `Vfs` that breaks the no-reuse contract, as the code itself documents.

### F7 NOTE (reasoned): AUTH_SYS identity is ignored by design

File: crates/cowfs-nfs/src/lib.rs:66-72, crates/cowfs-nfs/src/mount.rs:165.
The bind is 127.0.0.1 only.
Every holder of a valid handle acts with the daemon's rights, so F1 and F2 are the whole access boundary.
`check_peer_uid` cannot see the kernel client's socket and is off by default, as documented.

## Recommended order

1. Fix F1 and F2 together, with protocol tests for EXPORT content and for a second MNT after the first one succeeds.
2. Fix F3 before relying on `hard` mounts in multi-user settings.
3. Fix F4 and F5 as correctness work.
4. Run a follow-up pass on items 2 to 4 of the brief.
