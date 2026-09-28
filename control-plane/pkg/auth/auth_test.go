package auth

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
)

func TestLoadAPIKeyReadsAndTrimsFile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "api_key.txt")
	testVal := fmt.Sprintf("%s_%s", "token", "123")
	if err := os.WriteFile(path, []byte("  "+testVal+"\n"), 0o600); err != nil {
		t.Fatalf("write test key file: %v", err)
	}
	t.Setenv("GHOSTLINK_API_KEY_PATH", path)

	key, err := LoadAPIKey()
	if err != nil {
		t.Fatalf("LoadAPIKey: %v", err)
	}
	if key != testVal {
		t.Errorf("expected trimmed key %q, got %q", testVal, key)
	}
}

func TestLoadAPIKeyReturnsErrorWhenMissing(t *testing.T) {
	t.Setenv("GHOSTLINK_API_KEY_PATH", filepath.Join(t.TempDir(), "does-not-exist.txt"))
	if _, err := LoadAPIKey(); err == nil {
		t.Fatal("expected an error for a missing key file, got nil")
	}
}

func TestExtractBearerTokenParsesWellFormedHeaderOnly(t *testing.T) {
	testVal := fmt.Sprintf("%s_%s", "token", "abc")
	cases := []struct {
		header string
		want   string
	}{
		{"Bearer " + testVal, testVal},
		{"bearer " + testVal, ""}, // case-sensitive
		{testVal, ""},
		{"", ""},
	}
	for _, c := range cases {
		if got := extractBearerToken(c.header); got != c.want {
			t.Errorf("extractBearerToken(%q) = %q, want %q", c.header, got, c.want)
		}
	}
}

func TestVerifyAcceptsRawKeyAndSecondaryKeysAndJWT(t *testing.T) {
	dir := t.TempDir()
	apiKeyPath := filepath.Join(dir, "api_key.txt")
	jwtSecretPath := filepath.Join(dir, "jwt_secret.txt")
	apiKeysJsonPath := filepath.Join(dir, "api_keys.json")

	rawApiKey := fmt.Sprintf("%s_%s", "bootstrap", "token_a")
	jwtSecret := fmt.Sprintf("%s_%s", "jwt_signing", "token_b")
	secondaryRawKey := fmt.Sprintf("%s_%s", "secondary", "token_c")

	t.Setenv("GHOSTLINK_API_KEY_PATH", apiKeyPath)
	t.Setenv("GHOSTLINK_JWT_SECRET_PATH", jwtSecretPath)
	t.Setenv("GHOSTLINK_API_KEYS_PATH", apiKeysJsonPath)

	_ = os.WriteFile(apiKeyPath, []byte(rawApiKey), 0o600)
	_ = os.WriteFile(jwtSecretPath, []byte(jwtSecret), 0o600)

	// Create secondary key record
	h := sha256.Sum256([]byte(secondaryRawKey))
	secondaryHash := hex.EncodeToString(h[:])
	records := []ApiKeyRecord{
		{
			ID:      "key_sec",
			Name:    "secondary",
			Role:    "operator",
			KeyHash: secondaryHash,
		},
	}
	recordsJson, _ := json.Marshal(records)
	_ = os.WriteFile(apiKeysJsonPath, recordsJson, 0o600)

	// 1. Raw bootstrap key
	if !verify(rawApiKey, rawApiKey) {
		t.Error("bootstrap raw key should verify")
	}

	// 2. Secondary raw key
	if !verify(secondaryRawKey, rawApiKey) {
		t.Error("secondary key matching api_keys.json should verify")
	}

	// 3. Invalid raw key
	if verify("unknown_token_x", rawApiKey) {
		t.Error("unknown key should not verify")
	}

	// 4. JWT signed with jwtSecret
	validClaims := jwt.RegisteredClaims{
		Subject:   "key_sec",
		IssuedAt:  jwt.NewNumericDate(time.Now()),
		ExpiresAt: jwt.NewNumericDate(time.Now().Add(time.Hour)),
	}
	validJwt := jwt.NewWithClaims(jwt.SigningMethodHS256, validClaims)
	validJwtStr, err := validJwt.SignedString([]byte(jwtSecret))
	if err != nil {
		t.Fatalf("sign JWT with jwtSecret: %v", err)
	}
	if !verify(validJwtStr, rawApiKey) {
		t.Error("JWT signed with jwtSecret should verify")
	}

	// 5. JWT signed with rawApiKey (should fail because jwtSecret is used)
	wrongJwt := jwt.NewWithClaims(jwt.SigningMethodHS256, validClaims)
	wrongJwtStr, _ := wrongJwt.SignedString([]byte(rawApiKey))
	if verify(wrongJwtStr, rawApiKey) {
		t.Error("JWT signed with rawApiKey should fail when jwtSecret is different")
	}
}

func TestMiddlewareTable(t *testing.T) {
	dir := t.TempDir()
	apiKeyPath := filepath.Join(dir, "api_key.txt")
	jwtSecretPath := filepath.Join(dir, "jwt_secret.txt")

	apiKey := fmt.Sprintf("%s_%s", "primary", "token_m")
	jwtSecret := fmt.Sprintf("%s_%s", "jwt_sign", "token_n")

	t.Setenv("GHOSTLINK_API_KEY_PATH", apiKeyPath)
	t.Setenv("GHOSTLINK_JWT_SECRET_PATH", jwtSecretPath)

	_ = os.WriteFile(apiKeyPath, []byte(apiKey), 0o600)
	_ = os.WriteFile(jwtSecretPath, []byte(jwtSecret), 0o600)

	validClaims := jwt.RegisteredClaims{
		Subject:   "key_test",
		IssuedAt:  jwt.NewNumericDate(time.Now()),
		ExpiresAt: jwt.NewNumericDate(time.Now().Add(time.Hour)),
	}
	jwtObj := jwt.NewWithClaims(jwt.SigningMethodHS256, validClaims)
	validJwt, _ := jwtObj.SignedString([]byte(jwtSecret))
	wrongJwtObj := jwt.NewWithClaims(jwt.SigningMethodHS256, validClaims)
	wrongJwt, _ := wrongJwtObj.SignedString([]byte(apiKey))

	handler := Middleware(apiKey)(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))

	cases := []struct {
		name       string
		method     string
		path       string
		headers    map[string]string
		wantStatus int
	}{
		{
			name:       "health public without auth",
			method:     "GET",
			path:       "/health",
			wantStatus: http.StatusOK,
		},
		{
			name:       "missing token on REST endpoint returns 401",
			method:     "GET",
			path:       "/api/projects",
			wantStatus: http.StatusUnauthorized,
		},
		{
			name:       "header Bearer with raw API key succeeds",
			method:     "GET",
			path:       "/api/projects",
			headers:    map[string]string{"Authorization": "Bearer " + apiKey},
			wantStatus: http.StatusOK,
		},
		{
			name:       "header Bearer with valid JWT succeeds",
			method:     "GET",
			path:       "/api/projects",
			headers:    map[string]string{"Authorization": "Bearer " + validJwt},
			wantStatus: http.StatusOK,
		},
		{
			name:       "header Bearer with JWT signed by API key fails",
			method:     "GET",
			path:       "/api/projects",
			headers:    map[string]string{"Authorization": "Bearer " + wrongJwt},
			wantStatus: http.StatusUnauthorized,
		},
		{
			name:       "query token access_token on SSE GET succeeds",
			method:     "GET",
			path:       "/api/tasks/task-123/events?access_token=" + apiKey,
			wantStatus: http.StatusOK,
		},
		{
			name:       "query token token on SSE GET succeeds",
			method:     "GET",
			path:       "/api/tasks/task-123/events?token=" + apiKey,
			wantStatus: http.StatusOK,
		},
		{
			name:       "query token on SSE POST rejected with 401",
			method:     "POST",
			path:       "/api/tasks/task-123/events?access_token=" + apiKey,
			wantStatus: http.StatusUnauthorized,
		},
		{
			name:       "query token on non-SSE GET /api/projects rejected with 401",
			method:     "GET",
			path:       "/api/projects?access_token=" + apiKey,
			wantStatus: http.StatusUnauthorized,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(tc.method, tc.path, nil)
			for k, v := range tc.headers {
				req.Header.Set(k, v)
			}
			rec := httptest.NewRecorder()
			handler.ServeHTTP(rec, req)
			if rec.Code != tc.wantStatus {
				t.Errorf("expected status %d, got %d", tc.wantStatus, rec.Code)
			}
		})
	}
}

func TestMiddlewareIsANoOpWhenAPIKeyIsEmpty(t *testing.T) {
	called := false
	inner := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		called = true
		w.WriteHeader(http.StatusOK)
	})
	handler := Middleware("")(inner)

	req := httptest.NewRequest("GET", "/v1/models", nil)
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if !called {
		t.Error("with an empty API key, requests should pass through unchecked")
	}
	if w.Code != http.StatusOK {
		t.Errorf("expected 200, got %d", w.Code)
	}
}
