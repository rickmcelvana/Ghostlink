#!/usr/bin/env bash
# scripts/run_agentic_tests.sh
# Comprehensive agentic inference test runner for Ghostlink
# Usage: bash scripts/run_agentic_tests.sh [--all|--quick|--profile|--concurrent]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test results tracker
PASSED=0
FAILED=0
SKIPPED=0

log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_pass() {
    echo -e "${GREEN}[PASS]${NC} $*"
    ((PASSED++))
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $*"
    ((FAILED++))
}

log_skip() {
    echo -e "${YELLOW}[SKIP]${NC} $*"
    ((SKIPPED++))
}

# Check backend health
check_backend() {
    log_info "Checking backend health..."
    for i in {1..30}; do
        if curl -f http://127.0.0.1:18014/health 2>/dev/null | grep -q healthy; then
            log_pass "Backend is healthy"
            return 0
        fi
        if [ $i -eq 30 ]; then
            log_fail "Backend did not become healthy after 30 attempts"
            return 1
        fi
        echo -n "."
        sleep 1
    done
}

# Run individual test
run_test() {
    local test_name="$1"
    local test_script="$2"
    shift 2
    local test_args=("$@")

    echo ""
    log_info "Running: $test_name"
    
    if [ ! -f "$test_script" ]; then
        log_skip "$test_script not found"
        return 2
    fi
    
    if python3 "$test_script" "${test_args[@]}"; then
        log_pass "$test_name completed"
        return 0
    else
        log_fail "$test_name failed"
        return 1
    fi
}

# Test suite: Quick (basic tests only, ~2 min)
test_quick() {
    echo ""
    echo "=========================================="
    echo "QUICK TEST SUITE (~2 minutes)"
    echo "=========================================="
    
    check_backend || return 1
    
    run_test "Streaming Inference" "scripts/test_streaming_inference.py" \
        --max-tokens 32 --min-tokens 3 --timeout 30 || true
    
    run_test "Session Continuity" "scripts/test_session_continuity.py" \
        --timeout 60 || true
    
    return 0
}

# Test suite: Standard (all core tests, ~5 min)
test_standard() {
    echo ""
    echo "=========================================="
    echo "STANDARD TEST SUITE (~5 minutes)"
    echo "=========================================="
    
    check_backend || return 1
    
    run_test "Streaming Inference" "scripts/test_streaming_inference.py" \
        --max-tokens 64 --min-tokens 5 --timeout 60 || true
    
    run_test "Session Continuity" "scripts/test_session_continuity.py" \
        --timeout 120 || true
    
    run_test "Tool Call Output" "scripts/test_tool_call_output.py" \
        --timeout 120 || true
    
    run_test "Token Budget" "scripts/test_token_budget.py" \
        --timeout 120 || true
    
    return 0
}

# Test suite: All (with advanced reliability, ~8 min)
test_all() {
    echo ""
    echo "=========================================="
    echo "FULL TEST SUITE (~8 minutes)"
    echo "=========================================="
    
    check_backend || return 1
    
    run_test "Streaming Inference" "scripts/test_streaming_inference.py" \
        --max-tokens 64 --min-tokens 5 --timeout 60 || true
    
    run_test "Session Continuity" "scripts/test_session_continuity.py" \
        --timeout 120 || true
    
    run_test "Tool Call Output" "scripts/test_tool_call_output.py" \
        --timeout 120 || true
    
    run_test "Token Budget" "scripts/test_token_budget.py" \
        --timeout 120 || true
    
    run_test "Advanced Reliability" "scripts/test_advanced_reliability.py" \
        --concurrent 10 --reliability-runs 20 --timeout 5 || true
    
    return 0
}

# Test suite: Performance profile
test_profile() {
    echo ""
    echo "=========================================="
    echo "PERFORMANCE PROFILE (~3 minutes)"
    echo "=========================================="
    
    log_info "Performance profiling requires Cargo (Rust)"
    
    if command -v cargo &> /dev/null; then
        log_info "Cargo found, generating performance snapshot..."
        
        mkdir -p tmp/perf_snapshot
        
        if python3 scripts/flow_perf_snapshot.py \
            --runs 3 --modes tcp inmem --output-dir tmp/perf_snapshot; then
            
            log_info "Generating maturity profile with jitter metrics..."
            python3 scripts/perf_maturity_profile.py \
                --summary tmp/perf_snapshot/summary.json \
                --format markdown \
                --output-json tmp/perf_scorecard.json
            
            log_pass "Performance profile generated"
            return 0
        else
            log_fail "Performance snapshot failed"
            return 1
        fi
    else
        log_skip "Cargo not found (optional)"
        return 0
    fi
}

# Display test results summary
display_summary() {
    echo ""
    echo "=========================================="
    echo "TEST SUMMARY"
    echo "=========================================="
    echo -e "${GREEN}Passed:${NC}  $PASSED"
    echo -e "${RED}Failed:${NC}  $FAILED"
    echo -e "${YELLOW}Skipped:${NC} $SKIPPED"
    echo ""
    
    if [ $FAILED -eq 0 ]; then
        log_pass "All tests passed!"
        return 0
    else
        log_fail "$FAILED test(s) failed"
        return 1
    fi
}

# Main entry point
main() {
    local test_mode="${1:-standard}"
    
    log_info "Ghostlink Agentic Inference Test Suite"
    log_info "Mode: $test_mode"
    echo ""
    
    case "$test_mode" in
        quick)
            test_quick
            ;;
        standard)
            test_standard
            ;;
        all)
            test_all
            ;;
        profile)
            test_profile
            ;;
        concurrent)
            test_all
            test_profile
            ;;
        *)
            echo "Usage: bash scripts/run_agentic_tests.sh [quick|standard|all|profile|concurrent]"
            echo ""
            echo "Modes:"
            echo "  quick       - Basic tests only (~2 min)"
            echo "  standard    - Core tests (~5 min) [DEFAULT]"
            echo "  all         - All tests including reliability (~8 min)"
            echo "  profile     - Performance profile with jitter (~3 min)"
            echo "  concurrent  - All + performance profile (~10 min)"
            exit 1
            ;;
    esac
    
    display_summary
    exit $?
}

main "$@"
