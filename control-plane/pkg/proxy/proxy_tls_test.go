package proxy

import (
	"crypto/tls"
	"net/http"
	"net/http/httptest"
	"testing"
)

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

	p := NewChatProxy(backend.URL)

	tr, ok := p.Client.Transport.(*http.Transport)
	if !ok {
		t.Fatalf("expected an *http.Transport, got %T", p.Client.Transport)
	}
	if tr.TLSClientConfig == nil || !tr.TLSClientConfig.InsecureSkipVerify {
		t.Fatal("loopback https backend must skip verification (self-signed cert)")
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
	tr, ok := p.Client.Transport.(*http.Transport)
	if ok && tr.TLSClientConfig != nil && tr.TLSClientConfig.InsecureSkipVerify {
		t.Fatal("non-loopback https backend must keep certificate verification")
	}
}

// A plaintext http backend must be untouched.
func TestHTTPBackendHasNoTLSConfig(t *testing.T) {
	p := NewChatProxy("http://127.0.0.1:8003")
	if tr, ok := p.Client.Transport.(*http.Transport); ok && tr.TLSClientConfig != nil {
		if tr.TLSClientConfig.InsecureSkipVerify {
			t.Fatal("http backend must not carry an InsecureSkipVerify transport")
		}
	}
}

// Guard against the exact regression: an https loopback backend must not produce
// the 503 that forward() emits when client.Do errors.
func TestNoServiceUnavailableForReachableLoopbackTLS(t *testing.T) {
	backend := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("ok"))
	}))
	defer backend.Close()

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
