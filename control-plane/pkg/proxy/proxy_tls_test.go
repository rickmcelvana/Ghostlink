package proxy

import (
	"crypto/tls"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

// writeTempCert writes a httptest TLS server's certificate to a temp file and
// returns its path, so the pinned-pool path can be exercised without depending on
// ghost-link's on-disk cert being present.
func writeTempCert(t *testing.T, srv *httptest.Server) string {
	t.Helper()
	cert := srv.Certificate()
	if cert == nil {
		t.Fatal("test server has no certificate")
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "tls_cert.pem")
	f, err := os.Create(path)
	if err != nil {
		t.Fatalf("create temp cert: %v", err)
	}
	defer f.Close()
	if err := pem.Encode(f, &pem.Block{Type: "CERTIFICATE", Bytes: cert.Raw}); err != nil {
		t.Fatalf("encode temp cert: %v", err)
	}
	return path
}

// The backend serves TLS whenever settings.enable_tls is set, which includes
// loopback, and presents a self-signed cert. Before this was handled, every
// proxied request to an https loopback backend failed cert verification and came
// back as "Backend unreachable" (503).
func TestLoopbackHTTPSBackendIsProxied(t *testing.T) {
	backend := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true}`))
	}))
	defer backend.Close()

	// httptest serves its own throwaway cert, so point the pinned pool at that
	// cert instead of ghost-link's on-disk one. Same code path, real verification.
	oldPath := backendCertPath
	backendCertPath = writeTempCert(t, backend)
	defer func() { backendCertPath = oldPath }()

	p := NewChatProxy(backend.URL)

	tr, ok := p.Client.Transport.(*http.Transport)
	if !ok {
		t.Fatalf("expected an *http.Transport, got %T", p.Client.Transport)
	}
	cfg := tr.TLSClientConfig
	if cfg == nil {
		t.Fatal("loopback https backend must carry a TLS config pinning ghost-link's cert")
	}
	if cfg.InsecureSkipVerify {
		t.Fatal("verification must stay enabled; the cert is pinned, not trusted blindly")
	}
	if cfg.RootCAs == nil {
		t.Fatal("expected a pinned RootCAs pool holding ghost-link's self-signed cert")
	}
	if cfg.MinVersion < tls.VersionTLS12 {
		t.Fatalf("MinVersion too low: %x", cfg.MinVersion)
	}

	front := httptest.NewServer(http.HandlerFunc(p.HandleBackendProxy))
	defer front.Close()

	resp, err := http.Get(front.URL + "/health")
	if err != nil {
		t.Fatalf("proxied request failed: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200 through the proxy, got %d", resp.StatusCode)
	}
}

// The relaxation must not leak to a non-loopback https backend, where the cert
// is expected to be verifiable.
func TestNonLoopbackHTTPSKeepsVerification(t *testing.T) {
	p := NewChatProxy("https://192.0.2.10:8003")
	if tr, ok := p.Client.Transport.(*http.Transport); ok && tr.TLSClientConfig != nil {
		t.Fatal("non-loopback https backend must use default system roots, not a pinned pool")
	}
}

// A plaintext http backend must be untouched.
func TestHTTPBackendHasNoTLSConfig(t *testing.T) {
	p := NewChatProxy("http://127.0.0.1:8003")
	if tr, ok := p.Client.Transport.(*http.Transport); ok && tr.TLSClientConfig != nil {
		t.Fatal("http backend must not carry a TLS config at all")
	}
}

// Guard against the exact regression: an https loopback backend must not produce
// the 503 that forward() emits when client.Do errors.
func TestNoServiceUnavailableForReachableLoopbackTLS(t *testing.T) {
	backend := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("ok"))
	}))
	defer backend.Close()

	oldPath := backendCertPath
	backendCertPath = writeTempCert(t, backend)
	defer func() { backendCertPath = oldPath }()

	p := NewChatProxy(backend.URL)
	req := httptest.NewRequest(http.MethodGet, "/api/metrics", nil)
	rec := httptest.NewRecorder()
	p.HandleBackendProxy(rec, req)

	if rec.Code == http.StatusServiceUnavailable {
		t.Fatalf("proxy returned 503 for a reachable loopback TLS backend: %q", rec.Body.String())
	}
	if rec.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d", rec.Code)
	}
	_ = tls.VersionTLS12 // keep the crypto/tls import meaningful for future cases
}
