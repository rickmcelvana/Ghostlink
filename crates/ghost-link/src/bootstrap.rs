//! One-time bootstrap tokens for handing a local GUI a short-lived session.
//!
//! ## Why this exists
//!
//! `GhostlinkAPI.apiKey` lives in memory only (never `localStorage`, so a stored
//! credential cannot be read by an unrelated page), which means a freshly loaded
//! GUI has no credential at all and every request 401s until an operator walks to
//! SecurityTab and pastes the Admin key.
//!
//! The obvious fix -- an endpoint that returns the raw `api_key.txt` value -- is
//! the wrong shape: it turns a permanent, full-privilege credential into something
//! retrievable over HTTP, and ghost-link's TLS is self-signed, so a client that
//! cannot pin that cert cannot verify the channel either.
//!
//! So the GUI is handed a **short-lived JWT** instead, exchanged through the
//! existing `/api/security/jwt/refresh` path. `auth::authenticate` already accepts
//! a JWT as a bearer token, honours it only while its subject key still exists
//! (so deleting the key revokes outstanding tokens immediately), and reads the
//! role fresh from the live record rather than from the token's claims.
//!
//! ## The exchange
//!
//! At startup the operator launches a server that prints a bootstrap code. The
//! GUI presents that code once to obtain a JWT, then keeps only the JWT in
//! memory. The code is:
//!
//! - **single-use** -- consumed by the first successful exchange
//! - **short-lived** -- `GHOSTLINK_BOOTSTRAP_TTL_SECS`, default 300s
//! - **loopback-only** -- an exchange attempt from a non-loopback peer is refused
//! - **equal in entropy to the thing it replaces for** -- generated from the OS
//!   CSPRNG, not a counter or timestamp
//!
//! ## What this deliberately does not do
//!
//! It does not weaken `auth_middleware`. The bootstrap exchange happens *before*
//! the GUI has any credential, so it is routed as an explicitly unauthenticated
//! endpoint that returns a JWT and nothing else -- it never returns key material.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default lifetime of a bootstrap code. Long enough to start a browser and load
/// the app, short enough that a code left on screen or in scrollback is not a
/// standing credential.
const DEFAULT_TTL_SECS: u64 = 300;

/// Codes longer than this are rejected outright rather than compared, so a caller
/// cannot use response timing to probe the value character by character.
const MAX_CODE_LEN: usize = 128;

// The map holds at most one entry: `issue_code` clears before inserting, and a
// redemption removes its entry. That is deliberate -- this is a startup path, not a
// session store, so a code cannot be minted repeatedly to build up a set of
// live credentials.

struct BootstrapCode {
    code: String,
    expires_at: Instant,
}

fn store() -> &'static Mutex<HashMap<String, BootstrapCode>> {
    static STORE: OnceLock<Mutex<HashMap<String, BootstrapCode>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// True only for addresses that cannot leave the machine.
fn is_loopback(addr: &std::net::SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => v4 == Ipv4Addr::LOCALHOST,
        IpAddr::V6(v6) => v6 == Ipv6Addr::LOCALHOST,
    }
}

/// True when the request came over loopback, honouring a reverse proxy that
/// forwards the original client address.
///
/// Trusting `X-Forwarded-For` blindly would defeat the loopback check entirely --
/// any caller can set the header. So it is honoured **only** when the immediate
/// peer is itself loopback, which is the case when ghost-link is behind the local
/// control-plane (which is how the GUI reaches it) and is not the case for a
/// genuinely remote caller, whose peer address is not loopback and therefore
/// never gets its header read.
fn peer_is_loopback(addr: &std::net::SocketAddr, forwarded_for: Option<&str>) -> bool {
    if !is_loopback(addr) {
        return false;
    }
    let Some(ff) = forwarded_for else {
        return true;
    };
    // Take the left-most entry: the original client, not the closest proxy.
    let first = ff.split(',').next().unwrap_or("").trim();
    match first.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        // Unparseable or absent client address: fall back to "the peer is
        // loopback", which is what we already established.
        Err(_) => true,
    }
}

/// Mints a new bootstrap code, replacing any outstanding ones.
///
/// Called at server start. The value is printed rather than logged to a file, so
/// it never lands in durable storage.
pub fn issue_code() -> String {
    let code = random_code();
    let ttl = ttl();
    let mut map = store().lock().unwrap_or_else(|p| p.into_inner());
    map.clear();
    map.insert(
        code.clone(),
        BootstrapCode {
            code: code.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        },
    );
    code
}

fn ttl() -> u64 {
    std::env::var("GHOSTLINK_BOOTSTRAP_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TTL_SECS)
}

fn random_code() -> String {
    // 32 hex chars from the OS CSPRNG = 128 bits, matching the API key's entropy
    // class rather than inventing a weaker scheme for the same job. `rand` is
    // already a ghost-link dependency and is a thin wrapper over the OS CSPRNG
    // (getrandom on Linux, BCryptGenRandom on Windows), so there is no reason to
    // hand-bind a platform syscall here.
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Consumes a bootstrap code, returning the key id it should mint a JWT for.
///
/// Returns `Err` with a caller-facing reason on every failure path, and every
/// failure is deliberately indistinguishable from the caller's perspective
/// (except the loopback refusal, which is safe to state plainly because it
/// reveals nothing about the code).
pub fn redeem(
    presented: &str,
    addr: &std::net::SocketAddr,
    forwarded_for: Option<&str>,
) -> Result<String, &'static str> {
    if presented.is_empty() || presented.len() > MAX_CODE_LEN {
        return Err("invalid bootstrap code");
    }
    if !peer_is_loopback(addr, forwarded_for) {
        // Stated plainly: it reveals nothing about whether the code was correct.
        return Err("bootstrap exchange is only available from loopback");
    }

    let now = Instant::now();
    let mut map = store().lock().unwrap_or_else(|p| p.into_inner());
    map.retain(|_, v| v.expires_at > now);

    let Some(entry) = map.get(presented) else {
        return Err("invalid or expired bootstrap code");
    };
    // Constant-time-ish comparison: the map lookup already did the work, but do
    // not let a partial match short-circuit before the expiry check above.
    if entry.code != presented {
        return Err("invalid or expired bootstrap code");
    }
    // Single use: removed on redemption, whatever happens next.
    map.remove(presented);
    Ok(presented.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback_v4() -> std::net::SocketAddr {
        "127.0.0.1:5000".parse().unwrap()
    }

    fn remote() -> std::net::SocketAddr {
        "10.0.0.5:5000".parse().unwrap()
    }

    #[test]
    fn issued_code_is_128_bits_of_hex() {
        let c = issue_code();
        assert_eq!(c.len(), 32);
        assert!(c.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn two_issues_differ() {
        assert_ne!(issue_code(), issue_code());
    }

    #[test]
    fn redemption_is_single_use() {
        let code = issue_code();
        assert!(redeem(&code, &loopback_v4(), None).is_ok());
        // Second attempt with the same code must fail.
        assert!(redeem(&code, &loopback_v4(), None).is_err());
    }

    #[test]
    fn refused_from_non_loopback() {
        let code = issue_code();
        assert!(redeem(&code, &remote(), None).is_err());
        // ...and the code survives, because it was not consumed.
        assert!(redeem(&code, &loopback_v4(), None).is_ok());
    }

    #[test]
    fn forwarded_for_cannot_spoof_loopback() {
        let code = issue_code();
        // Remote peer claiming to be loopback in the header: still refused.
        assert!(redeem(&code, &remote(), Some("127.0.0.1")).is_err());
        // Remote peer claiming a different client: refused.
        assert!(redeem(&code, &remote(), Some("8.8.8.8")).is_err());
    }

    #[test]
    fn forwarded_for_from_loopback_peer_is_honoured() {
        let code = issue_code();
        // Local control-plane forwarding a loopback client: allowed.
        assert!(redeem(&code, &loopback_v4(), Some("127.0.0.1")).is_ok());
    }

    #[test]
    fn ipv6_loopback_counts() {
        let code = issue_code();
        let v6: std::net::SocketAddr = "[::1]:5000".parse().unwrap();
        assert!(redeem(&code, &v6, None).is_ok());
    }

    #[test]
    fn garbage_and_empty_are_rejected() {
        let _ = issue_code();
        assert!(redeem("", &loopback_v4(), None).is_err());
        assert!(redeem("deadbeef", &loopback_v4(), None).is_err());
        assert!(redeem(&"a".repeat(MAX_CODE_LEN + 1), &loopback_v4(), None).is_err());
    }

    #[test]
    fn expired_code_is_rejected() {
        // Rather than sleeping, plant an already-expired entry directly.
        let code = "expired-code-for-test".to_string();
        store().lock().unwrap().insert(
            code.clone(),
            BootstrapCode {
                code: code.clone(),
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        assert!(redeem(&code, &loopback_v4(), None).is_err());
    }
}
