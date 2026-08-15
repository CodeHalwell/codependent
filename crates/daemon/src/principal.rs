//! The server-derived principal on a connection (outcome 19).
//!
//! Before this module the daemon had no principal: `Envelope.client_id` and
//! `AttachSession.requested_role` were plaintext assertions the client made
//! about itself, and every `UserId` in the ledger was manufactured from the
//! client's own UUID. A fresh socket connection could therefore read any
//! session's history and resolve any parked approval.
//!
//! This is a local-first product served over a Unix domain socket, so the
//! proportionate answer is the one the transport already gives us for free:
//! `SO_PEERCRED`. The kernel stamps the connecting process's uid/gid/pid onto
//! the socket at `connect(2)`; the peer cannot set it, cannot spoof it, and
//! cannot change it afterwards. It is the only fact about a caller that is not
//! attacker-controlled, and therefore the only sound basis for identity here.
//!
//! `client_id` survives as a *correlation* token — reconnect, presence,
//! attribution and idempotency all key off it, and none of those is authority.
//! It no longer confers identity.

use codypendent_protocol::ids::UserId;
use tokio::net::UnixStream;

/// Who is on the other end of a connection, as reported by the kernel.
///
/// Constructed once, at accept time, from the transport — never from a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerPrincipal {
    uid: u32,
    gid: u32,
    /// Diagnostics only. A pid is racy — the process can exit and the number be
    /// recycled — so it is logged and authorizes nothing.
    pid: Option<i32>,
}

impl PeerPrincipal {
    /// Read the peer's credentials off a connected Unix socket.
    ///
    /// On Linux this is `getsockopt(SOL_SOCKET, SO_PEERCRED)`; tokio wraps the
    /// per-platform equivalent (`LOCAL_PEERCRED` / `getpeereid`) behind the same
    /// call, so the daemon does not need a per-OS branch here. A failure is
    /// propagated rather than defaulted: for a connected `AF_UNIX` socket this
    /// cannot legitimately fail, so a failure means something is wrong that we
    /// do not understand — and the caller closes the connection.
    pub fn from_stream(stream: &UnixStream) -> std::io::Result<Self> {
        let cred = stream.peer_cred()?;
        Ok(Self {
            uid: cred.uid(),
            gid: cred.gid(),
            pid: cred.pid(),
        })
    }

    /// A principal for a specific uid, for in-process callers that already hold
    /// a server-derived uid (the Remote-UI broker re-derives a session's owner
    /// from storage) and for tests. There is no path from the wire to here:
    /// nothing deserializes into a `PeerPrincipal`.
    #[must_use]
    pub const fn from_uid(uid: u32) -> Self {
        Self {
            uid,
            gid: uid,
            pid: None,
        }
    }

    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    #[must_use]
    pub const fn gid(&self) -> u32 {
        self.gid
    }

    #[must_use]
    pub const fn pid(&self) -> Option<i32> {
        self.pid
    }

    /// The ledger identity for this principal. Every `Actor::Human` the daemon
    /// records is minted from here, so an approval's `resolved_by` names the OS
    /// user that actually resolved it instead of a UUID the caller chose.
    #[must_use]
    pub fn user_id(&self) -> UserId {
        UserId(format!("uid:{}", self.uid))
    }

    /// Whether this principal may act on a resource owned by `owner_uid`.
    ///
    /// Same uid is the owner, and deliberately so: a process running as the same
    /// OS user can already read the daemon's database, ptrace the daemon and
    /// rewrite its config, so refusing it at the socket would buy nothing while
    /// breaking every legitimate client. The defect being fixed is that a
    /// *different* principal was accepted too — that anything at all was.
    #[must_use]
    pub const fn owns(&self, owner_uid: u32) -> bool {
        self.uid == owner_uid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_is_derived_from_the_uid_not_a_client_supplied_value() {
        assert_eq!(PeerPrincipal::from_uid(1000).user_id().0, "uid:1000");
        assert_ne!(
            PeerPrincipal::from_uid(1000).user_id(),
            PeerPrincipal::from_uid(1001).user_id()
        );
    }

    #[test]
    fn a_principal_owns_only_its_own_uid() {
        let principal = PeerPrincipal::from_uid(1000);
        assert!(principal.owns(1000));
        assert!(!principal.owns(0));
        assert!(!principal.owns(1001));
    }

    #[tokio::test]
    async fn peer_credentials_come_from_the_transport() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("peercred.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let cred = PeerPrincipal::from_stream(&stream).expect("peer cred");
            (stream, cred)
        });
        let client = UnixStream::connect(&path).await.expect("connect");
        let (_server_stream, server_side) = accepted.await.expect("join");
        let client_side = PeerPrincipal::from_stream(&client).expect("peer cred");
        // Both ends are this test process, so the kernel reports the same uid on
        // both sides — the point being that neither end supplied it.
        assert_eq!(server_side.uid(), client_side.uid());
        assert!(server_side.pid().is_some());
    }
}
