//! Transport abstraction layer for Ghostlink pipeline execution.
//!
//! Provides a unified `Transport` trait implemented by TCP, Unix domain socket,
//! and (feature-gated / experimental) AF_XDP backends.

use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::xdp::{probe_xdp_support, XdpUnavailable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportHealth {
    Healthy,
    Degraded(String),
    Failed(String),
}

pub trait Transport: Send + Sync + Debug {
    fn send_frame(&self, frame: &[u8]) -> io::Result<()>;
    fn recv_frame(&self, buf: &mut [u8]) -> io::Result<Option<usize>>;
    fn health(&self) -> TransportHealth;
    fn name(&self) -> &'static str;
}

/// TCP transport implementation.
#[derive(Debug)]
pub struct TcpTransport {
    stream: Mutex<TcpStream>,
}

impl TcpTransport {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream: Mutex::new(stream),
        }
    }
}

impl Transport for TcpTransport {
    fn send_frame(&self, frame: &[u8]) -> io::Result<()> {
        let mut s = self
            .stream
            .lock()
            .map_err(|_| io::Error::other("lock poisoned on TcpTransport send"))?;
        s.write_all(frame)?;
        s.flush()
    }

    fn recv_frame(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let mut s = self
            .stream
            .lock()
            .map_err(|_| io::Error::other("lock poisoned on TcpTransport recv"))?;
        match s.read(buf) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn health(&self) -> TransportHealth {
        let s = match self.stream.lock() {
            Ok(guard) => guard,
            Err(_) => return TransportHealth::Failed("Lock poisoned".into()),
        };
        match s.peer_addr() {
            Ok(_) => TransportHealth::Healthy,
            Err(e) => TransportHealth::Failed(e.to_string()),
        }
    }

    fn name(&self) -> &'static str {
        "tcp"
    }
}

/// Unix domain socket transport implementation.
#[cfg(unix)]
#[derive(Debug)]
pub struct UnixTransport {
    stream: Mutex<UnixStream>,
}

#[cfg(unix)]
impl UnixTransport {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream: Mutex::new(stream),
        }
    }
}

#[cfg(unix)]
impl Transport for UnixTransport {
    fn send_frame(&self, frame: &[u8]) -> io::Result<()> {
        let mut s = self
            .stream
            .lock()
            .map_err(|_| io::Error::other("lock poisoned on UnixTransport send"))?;
        s.write_all(frame)?;
        s.flush()
    }

    fn recv_frame(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let mut s = self
            .stream
            .lock()
            .map_err(|_| io::Error::other("lock poisoned on UnixTransport recv"))?;
        match s.read(buf) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn health(&self) -> TransportHealth {
        TransportHealth::Healthy
    }

    fn name(&self) -> &'static str {
        "unix"
    }
}

/// AF_XDP experimental transport implementation.
#[derive(Debug)]
pub struct XdpTransport {
    #[allow(dead_code)]
    interface_name: String,
    failed: Mutex<Option<String>>,
}

impl XdpTransport {
    pub fn new(interface_name: &str) -> Result<Self, XdpUnavailable> {
        probe_xdp_support(interface_name)?;
        Ok(Self {
            interface_name: interface_name.to_string(),
            failed: Mutex::new(None),
        })
    }

    pub fn mark_failed(&self, reason: String) {
        if let Ok(mut f) = self.failed.lock() {
            *f = Some(reason);
        }
    }
}

impl Transport for XdpTransport {
    fn send_frame(&self, _frame: &[u8]) -> io::Result<()> {
        if let Ok(guard) = self.failed.lock() {
            if let Some(reason) = guard.as_ref() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    format!("XDP transport degraded: {reason}"),
                ));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "AF_XDP send_frame requires Linux kernel AF_XDP socket setup (experimental)",
        ))
    }

    fn recv_frame(&self, _buf: &mut [u8]) -> io::Result<Option<usize>> {
        if let Ok(guard) = self.failed.lock() {
            if let Some(reason) = guard.as_ref() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    format!("XDP transport degraded: {reason}"),
                ));
            }
        }
        Ok(None)
    }

    fn health(&self) -> TransportHealth {
        if let Ok(guard) = self.failed.lock() {
            if let Some(reason) = guard.as_ref() {
                return TransportHealth::Failed(reason.clone());
            }
        }
        TransportHealth::Healthy
    }

    fn name(&self) -> &'static str {
        "xdp"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    Auto,
    Tcp,
    Xdp,
}

impl TransportMode {
    pub fn from_env() -> Self {
        if let Ok(val) = std::env::var("GHOSTLINK_TRANSPORT") {
            match val.trim().to_lowercase().as_str() {
                "tcp" => TransportMode::Tcp,
                "xdp" => TransportMode::Xdp,
                _ => TransportMode::Auto,
            }
        } else {
            TransportMode::Auto
        }
    }
}

pub fn is_transport_strict_from_env() -> bool {
    if let Ok(val) = std::env::var("GHOSTLINK_TRANSPORT_STRICT") {
        matches!(val.trim().to_lowercase().as_str(), "1" | "true" | "yes")
    } else {
        false
    }
}

#[derive(Debug)]
pub struct TransportMetrics {
    pub active_transport: Mutex<String>,
    pub failovers_total: AtomicU64,
    pub xdp_last_error: Mutex<Option<String>>,
}

impl Default for TransportMetrics {
    fn default() -> Self {
        Self {
            active_transport: Mutex::new("tcp".to_string()),
            failovers_total: AtomicU64::new(0),
            xdp_last_error: Mutex::new(None),
        }
    }
}

impl TransportMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_active(&self, name: &str) {
        if let Ok(mut guard) = self.active_transport.lock() {
            *guard = name.to_string();
        }
    }

    pub fn get_active(&self) -> String {
        self.active_transport
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| "tcp".to_string())
    }

    pub fn record_failover(&self, reason: String) {
        self.failovers_total.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.xdp_last_error.lock() {
            *guard = Some(reason);
        }
        self.set_active("tcp");
    }

    pub fn get_last_error(&self) -> Option<String> {
        self.xdp_last_error.lock().ok().and_then(|g| g.clone())
    }
}

#[derive(Debug)]
pub struct TransportManager {
    mode: TransportMode,
    strict: bool,
    max_consecutive_errors: usize,
    consecutive_errors: Mutex<usize>,
    primary_xdp: Option<Arc<XdpTransport>>,
    fallback_tcp: Arc<TcpTransport>,
    metrics: Arc<TransportMetrics>,
    last_probe_attempt: Mutex<Instant>,
    current_backoff_secs: Mutex<u64>,
    stable_check_count: Mutex<usize>,
    xdp_active: Mutex<bool>,
}

impl TransportManager {
    pub fn new(
        mode: TransportMode,
        strict: bool,
        interface_name: &str,
        tcp_stream: TcpStream,
    ) -> Result<Self, XdpUnavailable> {
        let metrics = Arc::new(TransportMetrics::new());
        let fallback_tcp = Arc::new(TcpTransport::new(tcp_stream));

        let (primary_xdp, xdp_active) = match mode {
            TransportMode::Tcp => {
                metrics.set_active("tcp");
                (None, false)
            }
            TransportMode::Auto | TransportMode::Xdp => match XdpTransport::new(interface_name) {
                Ok(xdp) => {
                    metrics.set_active("xdp");
                    (Some(Arc::new(xdp)), true)
                }
                Err(err) => {
                    if strict {
                        return Err(err);
                    } else {
                        tracing::warn!(
                                "AF_XDP transport unavailable on interface '{}' ({err}); failing over to TCP",
                                interface_name
                            );
                        metrics.record_failover(err.to_string());
                        (None, false)
                    }
                }
            },
        };

        Ok(Self {
            mode,
            strict,
            max_consecutive_errors: 3,
            consecutive_errors: Mutex::new(0),
            primary_xdp,
            fallback_tcp,
            metrics,
            last_probe_attempt: Mutex::new(Instant::now()),
            current_backoff_secs: Mutex::new(2),
            stable_check_count: Mutex::new(0),
            xdp_active: Mutex::new(xdp_active),
        })
    }

    pub fn mode(&self) -> TransportMode {
        self.mode
    }

    pub fn is_strict(&self) -> bool {
        self.strict
    }

    pub fn metrics(&self) -> Arc<TransportMetrics> {
        Arc::clone(&self.metrics)
    }

    pub fn is_xdp_active(&self) -> bool {
        self.xdp_active.lock().map(|g| *g).unwrap_or(false)
    }

    pub fn handle_xdp_error(&self, reason: String) {
        let mut err_count = self
            .consecutive_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *err_count += 1;

        if *err_count >= self.max_consecutive_errors {
            let mut active = self.xdp_active.lock().unwrap_or_else(|e| e.into_inner());
            if *active {
                *active = false;
                if let Some(xdp) = &self.primary_xdp {
                    xdp.mark_failed(reason.clone());
                }
                tracing::warn!(
                    "AF_XDP transport hit {} consecutive errors ({reason}); degrading and failing over to TCP",
                    *err_count
                );
                self.metrics.record_failover(reason);
            }
        }
    }

    pub fn send_frame(&self, frame: &[u8]) -> io::Result<()> {
        if self.is_xdp_active() {
            if let Some(xdp) = &self.primary_xdp {
                match xdp.send_frame(frame) {
                    Ok(()) => {
                        if let Ok(mut err_count) = self.consecutive_errors.lock() {
                            *err_count = 0;
                        }
                        return Ok(());
                    }
                    Err(err) => {
                        self.handle_xdp_error(err.to_string());
                    }
                }
            }
        }
        self.fallback_tcp.send_frame(frame)
    }

    pub fn recv_frame(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        if self.is_xdp_active() {
            if let Some(xdp) = &self.primary_xdp {
                match xdp.recv_frame(buf) {
                    Ok(res) => {
                        if let Ok(mut err_count) = self.consecutive_errors.lock() {
                            *err_count = 0;
                        }
                        return Ok(res);
                    }
                    Err(err) => {
                        self.handle_xdp_error(err.to_string());
                    }
                }
            }
        }
        self.fallback_tcp.recv_frame(buf)
    }

    /// Re-probe XDP and fail back if healthy over a stable window of checks.
    pub fn try_reprobe_and_failback(&self, interface_name: &str) -> bool {
        if self.is_xdp_active() || self.primary_xdp.is_none() {
            return false;
        }

        let mut last_probe = self.last_probe_attempt.lock().unwrap();
        let mut backoff = self.current_backoff_secs.lock().unwrap();

        if last_probe.elapsed() < Duration::from_secs(*backoff) {
            return false;
        }

        *last_probe = Instant::now();

        match probe_xdp_support(interface_name) {
            Ok(()) => {
                let mut stable = self.stable_check_count.lock().unwrap();
                *stable += 1;
                if *stable >= 3 {
                    let mut active = self.xdp_active.lock().unwrap();
                    *active = true;
                    *stable = 0;
                    *backoff = 2;
                    if let Ok(mut err_count) = self.consecutive_errors.lock() {
                        *err_count = 0;
                    }
                    self.metrics.set_active("xdp");
                    tracing::info!(
                        "AF_XDP probe succeeded over stable window; failing back to XDP transport"
                    );
                    return true;
                }
            }
            Err(err) => {
                let mut stable = self.stable_check_count.lock().unwrap();
                *stable = 0;
                *backoff = (*backoff * 2).min(60);
                if let Ok(mut last_err) = self.metrics.xdp_last_error.lock() {
                    *last_err = Some(err.to_string());
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn transport_manager_mode_and_strict_behavior() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client_stream = TcpStream::connect(addr).unwrap();
        let _server_stream = listener.accept().unwrap();

        // 1. Non-strict mode with unavailable XDP -> falls back to TCP cleanly
        let mgr = TransportManager::new(
            TransportMode::Xdp,
            false,
            "nonexistent_iface_xyz",
            client_stream,
        )
        .expect("non-strict mode should fall back to TCP");

        assert!(!mgr.is_xdp_active());
        assert_eq!(mgr.metrics().get_active(), "tcp");
        assert!(mgr.metrics().get_last_error().is_some());

        // 2. Strict mode with unavailable XDP -> returns XdpUnavailable error
        let listener_strict = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr_strict = listener_strict.local_addr().unwrap();
        let client_stream_strict = TcpStream::connect(addr_strict).unwrap();
        let _server_stream_strict = listener_strict.accept().unwrap();

        let res = TransportManager::new(
            TransportMode::Xdp,
            true,
            "nonexistent_iface_xyz",
            client_stream_strict,
        );
        assert!(res.is_err());
    }

    #[test]
    fn transport_manager_failover_and_batch_ordering() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client_stream = TcpStream::connect(addr).unwrap();
        let (mut server_stream, _) = listener.accept().unwrap();

        let mut mgr =
            TransportManager::new(TransportMode::Tcp, false, "lo", client_stream).unwrap();

        // Manually simulate primary XDP active
        if let Ok(xdp) = XdpTransport::new("lo") {
            mgr.primary_xdp = Some(Arc::new(xdp));
            *mgr.xdp_active.lock().unwrap() = true;
            mgr.metrics().set_active("xdp");
        }

        let frame_payload = b"test_frame_payload";

        // If XDP is active, triggering errors should cause failover to TCP after 3 errors
        if mgr.is_xdp_active() {
            for i in 1..=3 {
                mgr.handle_xdp_error(format!("error {i}"));
            }
            assert!(!mgr.is_xdp_active());
            assert_eq!(mgr.metrics().get_active(), "tcp");
            assert_eq!(mgr.metrics().failovers_total.load(Ordering::Relaxed), 1);
        }

        // Frames sent now go via TCP fallback
        mgr.send_frame(frame_payload)
            .expect("send over TCP fallback should succeed");

        let mut recv_buf = vec![0u8; 64];
        let n = server_stream.read(&mut recv_buf).unwrap();
        assert_eq!(&recv_buf[..n], frame_payload);
    }
}
