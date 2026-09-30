//! Who is on the other end of a loopback connection, found with `lsof`.
use std::net::SocketAddr;
use std::process::Command;
use std::time::Duration;

use crate::mount::run;

const LSOF: &str = "/usr/sbin/lsof";

/// The owners `lsof -F pun` output names for the two ends of the connection.
fn owners(out: &str, peer: SocketAddr, local: SocketAddr) -> (Option<u32>, Option<u32>) {
    let peer_end = format!(
        "{}:{}->{}:{}",
        peer.ip(),
        peer.port(),
        local.ip(),
        local.port()
    );
    let local_end = format!(
        "{}:{}->{}:{}",
        local.ip(),
        local.port(),
        peer.ip(),
        peer.port()
    );
    let (mut uid, mut peer_uid, mut local_uid) = (None, None, None);
    for line in out.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => uid = None,
            "u" => uid = value.parse().ok(),
            "n" if value == peer_end => peer_uid = uid,
            "n" if value == local_end => local_uid = uid,
            _ => {}
        }
    }
    (peer_uid, local_uid)
}

/// False only if `lsof` positively shows that the process owning the peer end of the connection
/// runs as another user than this server. That is a narrow check: the macOS kernel NFS client
/// connects from a kernel socket that no process owns, and an unprivileged `lsof` cannot see other
/// users' sockets either, so in both cases nothing is found and the peer is allowed. Do not rely
/// on it. The protection that holds is the one-shot MNT and the keyed handle MACs.
pub fn same_user(peer: SocketAddr, local: SocketAddr) -> bool {
    let port = format!("-iTCP:{}", peer.port());
    let out = run(
        Command::new(LSOF).args(["-nP", "-a", &port, "-F", "pun"]),
        Duration::from_secs(10),
    );
    match out {
        // lsof exits 1 when it finds nothing
        Ok((o, stdout)) if matches!(o.status.code(), Some(0 | 1)) => {
            match owners(&stdout, peer, local) {
                (Some(p), Some(l)) => p == l,
                _ => true,
            }
        }
        _ => {
            eprintln!("cowfs-nfs: cannot check who mounts (lsof failed), allowing it");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn owners_are_read_from_lsof_output() {
        let out = "p10\nu501\nf4\nn127.0.0.1:5000->127.0.0.1:6000\np11\nu502\nf9\nn127.0.0.1:6000->127.0.0.1:5000\nf10\nn*:6000\n";
        assert_eq!(owners(out, a(5000), a(6000)), (Some(501), Some(502)));
        assert_eq!(owners(out, a(5001), a(6000)), (None, None));
        assert_eq!(owners("", a(1), a(2)), (None, None));
        assert_eq!(owners("garbage\n\nn\nuabc\n", a(1), a(2)), (None, None));
    }

    #[test]
    fn a_socket_of_this_process_belongs_to_this_user() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = l.local_addr().unwrap();
        let c = std::net::TcpStream::connect(local).unwrap();
        let (_s, peer) = l.accept().unwrap();
        assert_eq!(c.local_addr().unwrap(), peer);
        assert!(same_user(peer, local));
    }

    #[test]
    fn a_connection_nobody_is_seen_on_is_allowed() {
        assert!(
            same_user(a(1), a(2)),
            "kernel sockets and other users' sockets are invisible"
        );
    }
}
