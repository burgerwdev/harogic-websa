# Unified entry
.PHONY: run stop clean build test dev all help ci hw-test bench

run:      ## Start service
	./run.sh

stop:     ## Stop service
	./stop.sh

clean:    ## Clean caches/logs/artifacts (also removes node_modules)
	./clean.sh

build:    ## Frontend build
	./build.sh

test:     ## Test (backend pytest + frontend vitest)
	./test.sh

dev:      ## Stop, clean (deps kept), rebuild frontend, run with WEBSA_TRACE=1
	./stop.sh
	./clean.sh --keep-deps
	./build.sh
	WEBSA_TRACE=1 ./run.sh
	@echo "trace log: /tmp/websa.log  (tail -f /tmp/websa.log)"

ci:       ## Everything CI runs, locally (no hardware needed)
	./test.sh
	python3 tools/sync_version.py --check
	python3 tools/gen_frame_fixtures.py --check
	python3 tools/quality/architecture_guard.py
	./build.sh
	@echo "OK: same gates as .github/workflows/ci.yml"

hw-test:  ## Hardware-in-the-loop: tinySA CW + SWP/RTA smoke + UI state regression (SAN-90 required)
	@pgrep -f "web_sa.supervisor" >/dev/null || ./run.sh
	@# The smoke test needs std mode; a previous e2e run may have left SDR/RTA active.
	@curl -s -X POST http://127.0.0.1:$${WEBSA_PORT:-8080}/api/config \
		-H 'Content-Type: application/json' -d '{"cmd":"SET_MODE","mode":"std"}' >/dev/null || true
	python3 tools/hardware_smoke.py --tinysa-port $${TINYSA_PORT:-/dev/ttyACM0} \
		--configure-tinysa --frequency 100.2e6 --span 10e6 --duration 3
	python3 tools/command_sweep.py
	python3 tools/e2e/state_regression.py
	@echo "OK: hardware smoke + command sweep + UI state regression"

bench:    ## Performance baseline: compare against tools/bench_baseline.json (service running)
	@pgrep -f "web_sa.supervisor" >/dev/null || ./run.sh
	python3 tools/bench.py --check tools/bench_baseline.json

bench-record:  ## Re-record the performance baseline on this host
	python3 tools/bench.py --duration 4 --write-baseline tools/bench_baseline.json

all:      ## Build + test + run
	./build.sh && ./test.sh && ./run.sh

help:     ## Show help
	@grep -E '^[a-zA-Z_-]+:.*##' Makefile | awk 'BEGIN {FS = ":.*## "}; {printf "  %-13s %s\n", $$1, $$2}'
