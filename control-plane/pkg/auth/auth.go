// Package auth verifies the same bearer token ghost-link's own API server
// requires (see crates/ghost-link/src/auth.rs) — real edge-rejection, not
// just relying on the proxy transparently forwarding the Authorization
// header through to ghost-link (which it already does; this middleware
// stops unauthenticated traffic here too, before it's proxied at all).
//
// Accepts either the raw shared API key as the bearer token, a raw secondary
// API key from api_keys.json, or a JWT issued by ghost-link's
// /api/security/jwt/refresh (HS256, signed with jwt_secret.txt) — genuine
// signature verification via golang-jwt, not a shape-only check.
package auth

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"os"
	"strings"

	"github.com/golang-jwt/jwt/v5"
)

// ApiKeyRecord represents a key record in api_keys.json.
type ApiKeyRecord struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	Role    string `json:"role"`
	KeyHash string `json:"key_hash"`
}

// LoadAPIKey reads the API key from the same file ghost-link itself
// generates and persists on first run (default api_key.txt, overridable
// via GHOSTLINK_API_KEY_PATH).
func LoadAPIKey() (string, error) {
	path := os.Getenv("GHOSTLINK_API_KEY_PATH")
	if path == "" {
		path = "api_key.txt"
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(data)), nil
}

// LoadJWTSecret reads the secret used to verify JWTs (default jwt_secret.txt,
// overridable via GHOSTLINK_JWT_SECRET_PATH).
func LoadJWTSecret() []byte {
	path := os.Getenv("GHOSTLINK_JWT_SECRET_PATH")
	if path == "" {
		path = "jwt_secret.txt"
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	trimmed := strings.TrimSpace(string(data))
	if trimmed == "" {
		return nil
	}
	return []byte(trimmed)
}

func loadApiKeyHashes() map[string]bool {
	path := os.Getenv("GHOSTLINK_API_KEYS_PATH")
	if path == "" {
		path = "api_keys.json"
	}
	hashes := make(map[string]bool)
	data, err := os.ReadFile(path)
	if err != nil {
		return hashes
	}
	var records []ApiKeyRecord
	if err := json.Unmarshal(data, &records); err != nil {
		return hashes
	}
	for _, r := range records {
		if r.KeyHash != "" {
			hashes[r.KeyHash] = true
		}
	}
	return hashes
}

func hashKey(raw string) string {
	h := sha256.Sum256([]byte(raw))
	return hex.EncodeToString(h[:])
}

func extractBearerToken(header string) string {
	const prefix = "Bearer "
	if !strings.HasPrefix(header, prefix) {
		return ""
	}
	return strings.TrimSpace(strings.TrimPrefix(header, prefix))
}

func isSSEPath(path string) bool {
	return strings.HasPrefix(path, "/api/tasks/") && strings.HasSuffix(path, "/events")
}

func extractToken(r *http.Request) string {
	headerToken := extractBearerToken(r.Header.Get("Authorization"))
	if headerToken != "" {
		return headerToken
	}

	// Query tokens are allowed ONLY on GET requests to SSE paths
	if r.Method == http.MethodGet && isSSEPath(r.URL.Path) {
		q := r.URL.Query()
		if tok := q.Get("access_token"); tok != "" {
			return tok
		}
		if tok := q.Get("token"); tok != "" {
			return tok
		}
	}

	return ""
}

// verify reports whether token is an exact match for apiKey, matches a hash
// in api_keys.json, or is a valid JWT signed with jwt_secret.txt.
func verify(token, apiKey string) bool {
	if token == "" {
		return false
	}
	if apiKey != "" && token == apiKey {
		return true
	}

	// Check secondary keys in api_keys.json
	hashed := hashKey(token)
	if hashes := loadApiKeyHashes(); hashes[hashed] {
		return true
	}

	// Check JWT using jwt_secret.txt
	secret := LoadJWTSecret()
	if len(secret) == 0 {
		return false
	}

	claims := jwt.RegisteredClaims{}
	parsed, err := jwt.ParseWithClaims(token, &claims, func(t *jwt.Token) (interface{}, error) {
		return secret, nil
	}, jwt.WithValidMethods([]string{"HS256"}))
	return err == nil && parsed.Valid
}

// Middleware guards every route except /health behind the shared bearer
// token or query parameter on allowed SSE GET routes.
func Middleware(apiKey string) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		if apiKey == "" {
			return next
		}
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path == "/health" {
				next.ServeHTTP(w, r)
				return
			}

			token := extractToken(r)
			if !verify(token, apiKey) {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusUnauthorized)
				_, _ = w.Write([]byte(`{"error":{"message":"missing or invalid Authorization: Bearer <token>","type":"unauthorized"}}`))
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}
