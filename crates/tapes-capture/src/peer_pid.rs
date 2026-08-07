//! Per-OS "which PID owns this TCP loopback connection" lookup.
//!
//! The proxy listens on `127.0.0.1:51539` and accepts TCP connections;
//! every accept hands back the peer's `(addr, port)` 4-tuple from
//! kernel state. The Claude process on the other end of that connection
//! has the matching socket in its FD table. The lookup answers:
//! "given a candidate PID set (the filenames in `~/.claude/sessions/`)
//! and the peer's loopback address, which candidate owns the socket?"
//!
//! Implementation: [`netsock`] enumerates the TCP socket table; we match
//! on `local_addr`/`local_port` and check whether a candidate PID owns the
//! socket. Ownership resolution is OS-split because attributing a socket to
//! a PID costs differently per platform:
//!
//! * Linux — `NETLINK_INET_DIAG` lists sockets (with inodes and UID) cheaply,
//!   but mapping an inode back to a PID means walking `/proc/<pid>/fd`. paperd
//!   runs unprivileged, so a *system-wide* walk (what
//!   `netsock::get_sockets` does internally) hits `EACCES` on every process
//!   we don't own and floods the log. We instead enumerate sockets with
//!   [`netsock::iter_sockets_without_processes`] (no walk) and read only a
//!   bounded PID set: either the caller's known candidates, or for owner
//!   lookup, processes whose `/proc/<pid>` owner matches the socket UID.
//! * macOS — `proc_pidfdinfo` is already per-PID and there is no
//!   unprivileged-host permission storm, so we keep netsock's
//!   process-attached enumeration as-is.
//!
//! A small per-peer-address memoization sits in front of the netsock
//! scan: Claude's HTTP/2 multiplexes many requests over one TCP
//! connection, and the `(peer_ip, peer_port)` tuple is invariant for the
//! connection's lifetime. Caching the resolved PID for [`CACHE_TTL`]
//! turns "global socket-table scan per request" into "scan once,
//! HashMap lookup after." Cached PIDs are revalidated against the live
//! candidate set on every hit so a dead process whose port gets reused
//! inside the window can't masquerade as the original owner.
//!
//! The cache key is the whole [`SocketAddr`], not just the port. A port
//! number is only unique within an address family and interface
//! address: `127.0.0.1:54321` and `[::1]:54321` are different sockets
//! that can be owned by different processes at the same instant, and
//! the scan below treats them as different (see [`addr_matches`] —
//! native v6 does not match v4). Keying on the port alone let one
//! entry answer for both.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use netsock::family::AddressFamilyFlags;
use netsock::protocol::ProtocolFlags;

/// How long a resolved peer-address → PID mapping stays valid. Chosen to
/// align with the kernel's TIME_WAIT window (≈60 s on Linux/macOS):
/// a closed port can't be reassigned to a different process until
/// TIME_WAIT drains, so an entry that survives TIME_WAIT cannot
/// out-live the connection it described.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// How many times the retrying owner lookups ([`lookup_owner`],
/// [`lookup_owner_async`]) scan the socket table before conceding that
/// no process owns the peer socket. The peer socket is fully
/// established by the time `accept` hands us its 4-tuple, so an owner
/// always exists — but the enumeration underneath (per-PID
/// `proc_pidfdinfo` walks on macOS, a netlink dump plus `/proc` reads
/// on Linux) is not a consistent snapshot, and under concurrent load a
/// scan can transiently miss the socket or its owning process. A miss
/// here is therefore usually a race, not an answer, and a couple of
/// short retries convert it into the right PID instead of letting the
/// caller fall through to its fail-closed path.
pub const OWNER_SCAN_ATTEMPTS: u32 = 3;

/// Pause between owner-lookup scan attempts. Bounded worst case:
/// `(OWNER_SCAN_ATTEMPTS - 1) * OWNER_SCAN_BACKOFF` = 20 ms, paid only
/// when every scan misses — a first-try hit returns immediately and
/// never sleeps.
///
/// Public together with [`OWNER_SCAN_ATTEMPTS`] so a caller that
/// already owns a poll loop can apply the same policy around
/// [`lookup_owner_once`] with its own sleep, instead of nesting this
/// module's retry inside its own.
pub const OWNER_SCAN_BACKOFF: Duration = Duration::from_millis(10);

/// Result of a peer-PID lookup attempt.
pub struct PeerPidLookup {
    /// Matched PID, if any candidate owned the peer socket.
    pub pid: Option<i32>,
    /// How long the lookup took, in microseconds. Reported so callers
    /// (the recorder, future per-request tracing) can characterize
    /// p50/p99 latency on real traffic. Cache hits are measured too —
    /// they're not free, just very cheap.
    pub micros: u64,
}

#[derive(Clone, Copy)]
struct CacheEntry {
    pid: i32,
    when: Instant,
}

/// Process-global memo of peer socket address → owning PID. Keyed by
/// the complete [`SocketAddr`]: the IP is as load-bearing as the port,
/// since a port is only unique per address, and two loopback peers can
/// legitimately hold the same port on different addresses.
fn cache() -> &'static Mutex<HashMap<SocketAddr, CacheEntry>> {
    static C: OnceLock<Mutex<HashMap<SocketAddr, CacheEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up which of `candidates` owns the loopback TCP socket whose
/// peer endpoint is `peer`. The peer is the *agent's* side of the
/// connection (the client socket on the Claude process); paperd's
/// `accept` returns that 4-tuple directly.
///
/// Returns `Self::pid = None` when no candidate matches (caller falls
/// through to the `harness_id: unknown` path) or when the underlying
/// netsock enumeration errors out.
pub fn lookup(candidates: &[i32], peer: SocketAddr) -> PeerPidLookup {
    let started = Instant::now();
    let pid = cached_lookup(candidates, peer);
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    PeerPidLookup { pid, micros }
}

/// Look up the process that owns the accepted loopback peer socket.
///
/// On Linux this still avoids `netsock::get_sockets`: netlink gives us the
/// socket inode and UID, then we inspect only processes owned by that UID. That
/// keeps manual Codex proxy attribution working without crawling root-owned
/// fd tables.
///
/// A `None` from a single scan is retried (see [`OWNER_SCAN_ATTEMPTS`]):
/// the socket table enumeration is racy under concurrent load on both
/// platforms, and the peer socket provably exists. Callers' fail-closed
/// handling of `pid: None` is unchanged — exhausting the retries still
/// yields `None`.
///
/// The retry pauses with **`std::thread::sleep`**, so this variant is for
/// synchronous callers only. From async code use [`lookup_owner_async`]
/// (same policy, `tokio::time::sleep` between attempts), or — if the call
/// site already sits inside a retrying poll loop, like the attribution
/// pipeline's Codex lane — [`lookup_owner_once`], and let the outer loop
/// own the backoff.
pub fn lookup_owner(peer: SocketAddr) -> PeerPidLookup {
    let started = Instant::now();
    let pid = resolve_with_retry(
        OWNER_SCAN_ATTEMPTS,
        OWNER_SCAN_BACKOFF,
        || owner_scan(peer),
        std::thread::sleep,
    );
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    PeerPidLookup { pid, micros }
}

/// [`lookup_owner`] for async callers: identical scan and retry policy,
/// but the pause between attempts is `tokio::time::sleep`, so a miss
/// yields the worker instead of stalling every task scheduled on it.
///
/// The scans themselves still run inline — they are bounded kernel/procfs
/// reads, the same cost the non-retrying lookup always paid on this path.
/// Only the added waiting became a yield point.
pub async fn lookup_owner_async(peer: SocketAddr) -> PeerPidLookup {
    let started = Instant::now();
    let pid =
        resolve_with_retry_async(OWNER_SCAN_ATTEMPTS, OWNER_SCAN_BACKOFF, || owner_scan(peer))
            .await;
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    PeerPidLookup { pid, micros }
}

/// One un-retried owner scan, for callers that already own a retry loop.
///
/// The attribution pipeline's Codex lane is the motivating case: it polls
/// under its own deadline with an async sleep between rounds, so a
/// transient scan miss is naturally retried on the next round and a
/// nested sleep here would only stack delays inside its budget. Callers
/// without such a loop should prefer [`lookup_owner`] /
/// [`lookup_owner_async`], or drive [`OWNER_SCAN_ATTEMPTS`] ×
/// [`OWNER_SCAN_BACKOFF`] themselves.
pub fn lookup_owner_once(peer: SocketAddr) -> PeerPidLookup {
    let started = Instant::now();
    let pid = owner_scan(peer);
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    PeerPidLookup { pid, micros }
}

/// One socket-table scan for the peer's owner, per-OS.
fn owner_scan(peer: SocketAddr) -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        cached_lookup_owner(peer)
    }
    #[cfg(not(target_os = "linux"))]
    {
        scan_owner(peer)
    }
}

/// Run `scan` up to `attempts` times, pausing via `sleep` between misses.
/// The first `Some` wins immediately — a found-on-first-try lookup never
/// sleeps — and there is no sleep after the final miss, so the failure
/// path adds exactly `(attempts - 1) * backoff` of latency.
///
/// `sleep` is injected rather than hard-coded so the caller decides *how*
/// waiting happens (blocking for sync contexts, recorded for tests); the
/// async twin below owes its separate existence only to `.await` not
/// fitting through a closure argument.
fn resolve_with_retry(
    attempts: u32,
    backoff: Duration,
    mut scan: impl FnMut() -> Option<i32>,
    mut sleep: impl FnMut(Duration),
) -> Option<i32> {
    for attempt in 1..=attempts {
        if let Some(pid) = scan() {
            return Some(pid);
        }
        if attempt < attempts {
            sleep(backoff);
        }
    }
    None
}

/// [`resolve_with_retry`] with the pause as `tokio::time::sleep`. Kept
/// byte-for-byte parallel to the sync loop; the tests drive both through
/// the same scenarios so the twins cannot drift.
async fn resolve_with_retry_async(
    attempts: u32,
    backoff: Duration,
    mut scan: impl FnMut() -> Option<i32>,
) -> Option<i32> {
    for attempt in 1..=attempts {
        if let Some(pid) = scan() {
            return Some(pid);
        }
        if attempt < attempts {
            tokio::time::sleep(backoff).await;
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn cached_lookup_owner(peer: SocketAddr) -> Option<i32> {
    let candidates = same_uid_pids_for_peer_socket(peer)?;
    cached_lookup(&candidates, peer)
}

fn cached_lookup(candidates: &[i32], peer: SocketAddr) -> Option<i32> {
    let key = peer;

    // Cache-hit path: revalidate against `candidates`. A stale entry
    // for a PID the watcher has since dropped (process died, address
    // reassigned) would mis-attribute fresh connections to a dead
    // owner; checking membership on every hit closes that hole
    // without needing the watcher to notify the cache.
    {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = map.get(&key).copied() {
            if entry.when.elapsed() < CACHE_TTL && candidates.contains(&entry.pid) {
                return Some(entry.pid);
            }
            map.remove(&key);
        }
    }

    // Cache miss / stale: scan. Only `Some` results are memoized —
    // `None` from a cold race (UA matched Claude but the watcher
    // hasn't yet parsed the session file) must remain retryable, or
    // the whole connection gets pinned to `unknown` for the full
    // TTL.
    let pid = scan(candidates, peer)?;
    let mut map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.insert(
        key,
        CacheEntry {
            pid,
            when: Instant::now(),
        },
    );
    Some(pid)
}

fn scan(candidates: &[i32], peer: SocketAddr) -> Option<i32> {
    // Enumerate both address families: a v4 peer can appear as a
    // v4-mapped v6 socket on the harness side (and vice-versa). netsock
    // reports the kernel's `local_addr` verbatim, so `addr_matches`
    // handles both cases without the explicit v4-mapped-v6 dance the old
    // hand-rolled code did.
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;

    #[cfg(target_os = "linux")]
    {
        scan_linux(candidates, peer, af)
    }
    #[cfg(not(target_os = "linux"))]
    {
        scan_netsock_attached(candidates, peer, af)
    }
}

#[cfg(not(target_os = "linux"))]
fn scan_owner(peer: SocketAddr) -> Option<i32> {
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let sockets = netsock::get_sockets(af, ProtocolFlags::TCP).ok()?;
    let peer_port = peer.port();

    sockets.into_iter().find_map(|s| {
        if s.local_port() != peer_port || !addr_matches(s.local_addr(), peer.ip()) {
            return None;
        }
        s.processes
            .into_iter()
            .find_map(|process| i32::try_from(process.pid).ok())
    })
}

/// Linux: enumerate sockets without the system-wide `/proc` walk, then
/// resolve ownership against only the candidate PIDs.
#[cfg(target_os = "linux")]
fn scan_linux(candidates: &[i32], peer: SocketAddr, af: AddressFamilyFlags) -> Option<i32> {
    let (inode, _uid) = peer_socket_inode_uid(peer, af)?;

    // Ownership: read only the candidate PIDs' fd tables. Candidates are
    // Claude harness children (same uid as paperd), so `/proc/<cand>/fd`
    // is readable — no EACCES, and at most a handful of dirs instead of
    // every process on the host.
    candidates
        .iter()
        .copied()
        .find(|&cand| pid_owns_socket_inode(cand, inode))
}

#[cfg(target_os = "linux")]
fn same_uid_pids_for_peer_socket(peer: SocketAddr) -> Option<Vec<i32>> {
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let (_inode, uid) = peer_socket_inode_uid(peer, af)?;
    Some(pids_owned_by_uid(uid))
}

#[cfg(target_os = "linux")]
fn peer_socket_inode_uid(peer: SocketAddr, af: AddressFamilyFlags) -> Option<(u32, u32)> {
    // Netlink-only: lists sockets (with inodes and UID) but attaches no
    // process info, so it never touches `/proc/<pid>/fd` and never emits the
    // permission-denied warnings paperd would hit on processes it can't read.
    let sockets = netsock::iter_sockets_without_processes(af, ProtocolFlags::TCP).ok()?;

    let peer_port = peer.port();
    sockets.into_iter().find_map(|s| {
        let s = s.ok()?;
        (s.local_port() == peer_port && addr_matches(s.local_addr(), peer.ip()))
            .then_some((s.inode, s.uid))
    })
}

#[cfg(target_os = "linux")]
fn pids_owned_by_uid(uid: u32) -> Vec<i32> {
    use std::os::unix::fs::MetadataExt;

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let metadata = entry.metadata().ok()?;
            (metadata.uid() == uid).then_some(pid)
        })
        .collect()
}

/// True if PID `pid` holds an fd pointing at `socket:[inode]`. A PID we
/// can't read (gone, or not ours) simply doesn't match.
#[cfg(target_os = "linux")]
fn pid_owns_socket_inode(pid: i32, inode: u32) -> bool {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    let needle = format!("socket:[{inode}]");
    entries.flatten().any(|entry| {
        // Socket fd links are always ASCII `socket:[N]`, so a `to_str`
        // comparison is sufficient and avoids OsStr equality surprises.
        std::fs::read_link(entry.path())
            .ok()
            .is_some_and(|link| link.to_str() == Some(needle.as_str()))
    })
}

/// Non-Linux (macOS): keep netsock's process-attached enumeration —
/// `proc_pidfdinfo` is per-PID and there is no unprivileged-host
/// permission storm to avoid.
#[cfg(not(target_os = "linux"))]
fn scan_netsock_attached(
    candidates: &[i32],
    peer: SocketAddr,
    af: AddressFamilyFlags,
) -> Option<i32> {
    let sockets = netsock::get_sockets(af, ProtocolFlags::TCP).ok()?;

    let peer_port = peer.port();
    for s in sockets {
        if s.local_port() != peer_port {
            continue;
        }
        if !addr_matches(s.local_addr(), peer.ip()) {
            continue;
        }
        for &cand in candidates {
            if let Ok(pid_u32) = u32::try_from(cand)
                && s.is_owned_by_pid(pid_u32)
            {
                return Some(cand);
            }
        }
    }
    None
}

/// Accept either-family loopback equivalence: a v4 peer matches a
/// v4-mapped v6 socket address and vice-versa. netsock returns the
/// kernel-reported `local_addr` verbatim, so the unmap happens here.
fn addr_matches(local: std::net::IpAddr, peer: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    if local == peer {
        return true;
    }
    match (local, peer) {
        (IpAddr::V6(v6), IpAddr::V4(v4)) | (IpAddr::V4(v4), IpAddr::V6(v6)) => {
            v6.to_ipv4_mapped() == Some(v4)
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};

    /// Drop any cached entry for this peer so tests reusing the same
    /// ephemeral address (rare but possible) don't see leftover state.
    fn clear_cache_for(peer: SocketAddr) {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.remove(&peer);
    }

    fn lookup_until_pid(candidates: &[i32], peer: SocketAddr, expected: i32) -> PeerPidLookup {
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let got = lookup(candidates, peer);
            if got.pid == Some(expected) || Instant::now() >= deadline {
                return got;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn addr_matches_same_family() {
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert!(addr_matches(v4, v4));
        assert!(addr_matches(v6, v6));
    }

    #[test]
    fn addr_matches_v4_mapped_v6_either_direction() {
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let mapped = IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped());
        assert!(addr_matches(mapped, v4));
        assert!(addr_matches(v4, mapped));
    }

    #[test]
    fn addr_matches_rejects_native_v6_vs_v4() {
        assert!(!addr_matches(
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ));
    }

    /// End-to-end behavior: open a real loopback TCP connection,
    /// then ask `lookup` whose PID owns the client side. We're the
    /// owner, so passing our own PID as the sole candidate should
    /// match. Verifies the netsock integration (filter flags, address
    /// matching, ownership predicate) against the live kernel.
    #[test]
    fn lookup_finds_self_pid_for_live_loopback_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let peer = client.local_addr().unwrap();
        clear_cache_for(peer);
        let me = std::process::id() as i32;

        let got = lookup_until_pid(&[me], peer, me);
        assert_eq!(
            got.pid,
            Some(me),
            "expected self pid {me} to own loopback socket {peer}",
        );
    }

    #[test]
    fn lookup_owner_finds_self_pid_for_live_loopback_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let peer = client.local_addr().unwrap();
        clear_cache_for(peer);
        let me = std::process::id() as i32;

        let deadline = Instant::now() + Duration::from_millis(250);
        let got = loop {
            let got = lookup_owner(peer);
            if got.pid == Some(me) || Instant::now() >= deadline {
                break got;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            got.pid,
            Some(me),
            "expected owner lookup to find self pid {me} for loopback socket {peer}",
        );
    }

    /// The second lookup for the same peer must be served from cache:
    /// we tear down the live socket between calls, so a second netsock
    /// scan would no longer find our PID owning anything on that port.
    /// If the call still returns our PID, the cache is doing its job.
    #[test]
    fn second_lookup_hits_cache_after_socket_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let peer = {
            let client = TcpStream::connect(server_addr).unwrap();
            let (_server_side, _peer) = listener.accept().unwrap();
            let peer = client.local_addr().unwrap();
            clear_cache_for(peer);

            let me = std::process::id() as i32;
            assert_eq!(
                lookup_until_pid(&[me], peer, me).pid,
                Some(me),
                "primer scan failed"
            );
            peer
            // `client` drops here; the kernel tears the socket down.
        };

        let me = std::process::id() as i32;
        let got = lookup(&[me], peer);
        assert_eq!(
            got.pid,
            Some(me),
            "expected cache hit to keep returning our pid after socket closed",
        );
    }

    /// A cached PID that's no longer in the candidate set must NOT be
    /// returned — that's how the cache stays correct when a Claude
    /// process dies and its peer port is later reused by some other
    /// process the watcher has not blessed.
    #[test]
    fn cached_pid_dropped_from_candidates_is_invalidated() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let peer = client.local_addr().unwrap();
        clear_cache_for(peer);

        let me = std::process::id() as i32;
        assert_eq!(
            lookup_until_pid(&[me], peer, me).pid,
            Some(me),
            "primer scan failed"
        );

        // Second call with a candidate set that excludes us: the
        // cached entry must be ignored. We'll get None because the
        // re-scan won't find any of the (bogus) candidates owning
        // the socket either.
        let got = lookup(&[1], peer);
        assert_eq!(
            got.pid, None,
            "stale cached pid was returned despite being absent from candidates",
        );
    }

    /// Two peers can hold the same port on different addresses at the
    /// same instant: `127.0.0.1:N` and `[::1]:N` are distinct sockets,
    /// and [`addr_matches`] deliberately refuses to equate native v6
    /// with v4. Keyed by port alone, the cache collapsed them into one
    /// entry, so a second peer inherited the first peer's PID without
    /// any socket of its own ever having been resolved — silent
    /// mis-attribution, since the candidate-set revalidation cannot
    /// catch it (the PID is live and blessed, just not the owner).
    #[test]
    fn distinct_addresses_sharing_a_port_do_not_share_a_cache_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let v4_peer = client.local_addr().unwrap();
        // Same port, different address. We own no socket here, so a
        // correct lookup has nothing to resolve and must answer None.
        let v6_peer = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), v4_peer.port());
        clear_cache_for(v4_peer);
        clear_cache_for(v6_peer);

        let me = std::process::id() as i32;
        assert_eq!(
            lookup_until_pid(&[me], v4_peer, me).pid,
            Some(me),
            "primer scan failed",
        );

        assert_eq!(
            lookup(&[me], v6_peer).pid,
            None,
            "the cache entry for {v4_peer} answered for {v6_peer} — \
             the cache is keyed by port rather than by address",
        );

        // The entry that legitimately exists is untouched by the miss.
        assert_eq!(lookup(&[me], v4_peer).pid, Some(me));

        clear_cache_for(v4_peer);
    }

    /// Structural companion to the behavioural test above: the map
    /// itself holds one entry per address, so the same port on two
    /// addresses carries independent PIDs and independent expiry.
    /// Uses port 9 (discard), outside the ephemeral range, so a real
    /// socket in a concurrently running test cannot collide with it.
    #[test]
    fn cache_holds_one_entry_per_address_not_per_port() {
        let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9);
        let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9);

        {
            let mut map = cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.insert(
                v4,
                CacheEntry {
                    pid: 111,
                    when: Instant::now(),
                },
            );
            map.insert(
                v6,
                CacheEntry {
                    pid: 222,
                    when: Instant::now(),
                },
            );
            assert_eq!(map.get(&v4).map(|e| e.pid), Some(111));
            assert_eq!(map.get(&v6).map(|e| e.pid), Some(222));
        }

        clear_cache_for(v4);
        clear_cache_for(v6);
    }

    /// A scan that hits on the first attempt returns without ever
    /// sleeping or re-scanning — the common path pays nothing for the
    /// retry machinery. The injected sleeper records instead of
    /// sleeping, so the assertion is on behaviour, not wall time.
    #[test]
    fn retry_returns_first_hit_without_rescanning_or_sleeping() {
        let mut calls = 0;
        let mut sleeps: Vec<Duration> = Vec::new();
        let got = resolve_with_retry(
            3,
            Duration::from_millis(10),
            || {
                calls += 1;
                Some(42)
            },
            |pause| sleeps.push(pause),
        );
        assert_eq!(got, Some(42));
        assert_eq!(calls, 1, "a first-try hit must not trigger extra scans");
        assert!(sleeps.is_empty(), "a first-try hit must never sleep");
    }

    /// A transient miss (the concurrent-load race) is absorbed: the
    /// scan is retried until it resolves, with one backoff pause per
    /// miss, and the eventual owner is returned.
    #[test]
    fn retry_recovers_from_transient_scan_misses() {
        let mut calls = 0;
        let mut sleeps: Vec<Duration> = Vec::new();
        let got = resolve_with_retry(
            3,
            Duration::from_millis(10),
            || {
                calls += 1;
                (calls == 3).then_some(7)
            },
            |pause| sleeps.push(pause),
        );
        assert_eq!(got, Some(7));
        assert_eq!(calls, 3, "retry should re-scan until the owner appears");
        assert_eq!(
            sleeps,
            vec![Duration::from_millis(10); 2],
            "one backoff pause per miss that has an attempt after it"
        );
    }

    /// A persistent miss stays a miss: exactly `attempts` scans run
    /// with no sleep after the last one, then `None` comes back so
    /// callers' fail-closed handling engages unchanged.
    #[test]
    fn retry_exhausts_attempts_then_fails_closed() {
        let mut calls = 0;
        let mut sleeps: Vec<Duration> = Vec::new();
        let got = resolve_with_retry(
            3,
            Duration::from_millis(10),
            || {
                calls += 1;
                None
            },
            |pause| sleeps.push(pause),
        );
        assert_eq!(got, None);
        assert_eq!(calls, 3, "exactly `attempts` scans, no more, no fewer");
        assert_eq!(sleeps.len(), 2, "no sleep after the final miss");
    }

    /// The async twin under a paused clock: a transient miss recovers
    /// after exactly the backoff pauses the policy prescribes, and the
    /// waiting is tokio-time (auto-advanced here, a worker yield in
    /// production) rather than a blocked thread — a
    /// `std::thread::sleep` inside would hang a paused single-thread
    /// runtime's auto-advance, so this test doubles as the guard that
    /// the async path never blocks.
    #[tokio::test(start_paused = true)]
    async fn async_retry_recovers_from_transient_scan_misses_without_blocking() {
        let started = tokio::time::Instant::now();
        let mut calls = 0;
        let got = resolve_with_retry_async(3, Duration::from_millis(10), || {
            calls += 1;
            (calls == 3).then_some(7)
        })
        .await;
        assert_eq!(got, Some(7));
        assert_eq!(calls, 3);
        assert_eq!(
            started.elapsed(),
            Duration::from_millis(20),
            "two misses cost exactly two backoff pauses of tokio time"
        );
    }

    /// Async first-try hit: no re-scan and zero tokio-time elapsed.
    #[tokio::test(start_paused = true)]
    async fn async_retry_returns_first_hit_without_sleeping() {
        let started = tokio::time::Instant::now();
        let mut calls = 0;
        let got = resolve_with_retry_async(3, Duration::from_millis(10), || {
            calls += 1;
            Some(42)
        })
        .await;
        assert_eq!(got, Some(42));
        assert_eq!(calls, 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    /// Async exhaustion: exactly `attempts` scans, no pause after the
    /// final miss, and the same fail-closed `None`.
    #[tokio::test(start_paused = true)]
    async fn async_retry_exhausts_attempts_then_fails_closed() {
        let started = tokio::time::Instant::now();
        let mut calls = 0;
        let got = resolve_with_retry_async(3, Duration::from_millis(10), || {
            calls += 1;
            None
        })
        .await;
        assert_eq!(got, None);
        assert_eq!(calls, 3);
        assert_eq!(started.elapsed(), Duration::from_millis(20));
    }

    /// The three public owner lookups share one scan: against a live
    /// loopback socket, the async and single-shot variants find the
    /// same owner the sync variant does.
    #[tokio::test]
    async fn async_and_once_variants_find_the_same_live_owner() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let peer = client.local_addr().unwrap();
        clear_cache_for(peer);
        let me = std::process::id() as i32;

        // Both variants get the same wall-clock budget, because what is under
        // test is which *owner* they agree on — not how few scans it takes to
        // see it. A single socket-table scan is allowed to miss a socket that
        // exists: that is precisely why `lookup_owner` and `lookup_owner_async`
        // retry at all, and on a platform whose `owner_scan` is uncached
        // (everything but Linux) `lookup_owner_once` has nothing to fall back
        // on, so under a loaded test binary it misses often enough to matter.
        // Asserting a bare `lookup_owner_once` here was asserting that the
        // documented transient-miss case never happens.
        let deadline = Instant::now() + Duration::from_millis(250);
        let got = loop {
            let got = lookup_owner_async(peer).await;
            if got.pid == Some(me) || Instant::now() >= deadline {
                break got;
            }
        };
        assert_eq!(got.pid, Some(me), "async owner lookup missed {peer}");

        let deadline = Instant::now() + Duration::from_millis(250);
        let once = loop {
            let once = lookup_owner_once(peer);
            if once.pid == Some(me) || Instant::now() >= deadline {
                break once;
            }
        };
        assert_eq!(once.pid, Some(me), "single-shot owner lookup missed {peer}");
    }

    #[test]
    fn lookup_returns_none_when_no_candidate_matches() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let peer = client.local_addr().unwrap();
        clear_cache_for(peer);

        // PID 1 (init/launchd) won't own a socket we just opened.
        let got = lookup(&[1], peer);
        assert_eq!(got.pid, None);
    }

    /// Manual benchmark. Run with:
    ///
    /// ```text
    /// cargo test -p paper-daemon --release \
    ///     proxy::session::peer_pid::tests::bench_lookup_microseconds \
    ///     -- --ignored --nocapture
    /// ```
    ///
    /// Reports cold (first / cache-miss) vs warm (cache-hit) latency
    /// percentiles. Old baseline (hand-rolled FFI + `/proc/net/tcp`):
    /// macOS p99 ≈ 78 µs, Linux p50 ≈ 25 ms. Cold path on netsock is
    /// expected higher (macOS) or lower (Linux); warm path should be
    /// single-digit µs on both.
    #[test]
    #[ignore = "manual benchmark; opt in with --ignored"]
    fn bench_lookup_microseconds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(server_addr).unwrap();
        let (_server_side, _peer) = listener.accept().unwrap();
        let peer = client.local_addr().unwrap();
        let me = std::process::id() as i32;

        const N: usize = 200;
        let pct = |samples: &[u64], q: f64| {
            samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)]
        };

        // Cold: clear cache before each iteration so every sample
        // pays for a full netsock scan.
        let mut cold = Vec::with_capacity(N);
        for _ in 0..5 {
            clear_cache_for(peer);
            let _ = lookup(&[me], peer);
        }
        for _ in 0..N {
            clear_cache_for(peer);
            cold.push(lookup(&[me], peer).micros);
        }
        cold.sort_unstable();
        eprintln!(
            "peer_pid::lookup COLD µs over {N} iters: p50={} p90={} p99={} p999={} max={}",
            pct(&cold, 0.50),
            pct(&cold, 0.90),
            pct(&cold, 0.99),
            pct(&cold, 0.999),
            cold.last().copied().unwrap_or(0),
        );

        // Warm: prime once, then hammer the cached path.
        clear_cache_for(peer);
        let _ = lookup(&[me], peer);
        let mut warm = Vec::with_capacity(N);
        for _ in 0..N {
            warm.push(lookup(&[me], peer).micros);
        }
        warm.sort_unstable();
        eprintln!(
            "peer_pid::lookup WARM µs over {N} iters: p50={} p90={} p99={} p999={} max={}",
            pct(&warm, 0.50),
            pct(&warm, 0.90),
            pct(&warm, 0.99),
            pct(&warm, 0.999),
            warm.last().copied().unwrap_or(0),
        );
    }
}
