# Unified entry
.PHONY: run stop restart status clean build test dev all help ci hw-test bench e2e-fake bench-record viewport-baseline viewport-check wasm wasm-check wasm-test

# WASM artifact staleness: the artifacts are COMMITTED (the service and CI gate on them), so a
# machine without Rust still builds fine - make only re-runs the wasm build when a source is
# newer than the published artifact AND a toolchain exists.
WASM_DSP_SRC := $(shell find wasm/src -name '*.rs' 2>/dev/null) wasm/Cargo.toml wasm/build.sh
WASM_DFN_SRC := $(shell find wasm-dfn/src -name '*.rs' 2>/dev/null) wasm-dfn/Cargo.toml wasm-dfn/build.sh

run:      ## Start service
	./run.sh

stop:     ## Stop service
	./stop.sh

restart:  ## Restart service (stop + start)
	./stop.sh
	./run.sh

status:   ## Show service status (pid, uptime, memory, CPU, log path/size)
	./status.sh

clean:    ## Clean caches/logs/artifacts (also removes node_modules)
	./clean.sh

build: frontend/modern/public/dsp.wasm frontend/modern/public/dfn/df_bg.wasm  ## Full build: WASM cores (only if stale + Rust present) + frontend -> dist
	./build.sh

test:     ## Test (backend pytest + frontend vitest)
	./test.sh

dev:      ## Stop, clean (deps kept), rebuild frontend, run with WEBSA_TRACE=1
	./stop.sh
	./clean.sh --keep-deps
	$(MAKE) build
	WEBSA_TRACE=1 ./run.sh
	@echo "trace log: /tmp/websa.log  (tail -f /tmp/websa.log)"

ci:       ## The in-process gates CI runs, locally (no hardware; browser e2e: make e2e-fake)
	./test.sh
	python3 tools/sync_version.py --check
	python3 tools/gen_frame_fixtures.py --check
	python3 tools/gen_dsp_fixtures.py --check
	python3 tools/gen_ft8_fixtures.py --check
	python3 tools/check_dom_ids.py
	python3 tools/check_registrations.py
	python3 tools/check_docs_parity.py
	python3 tools/check_doc_paths.py
	python3 tools/check_wasm_artifact.py
	tools/build_ggmorse_wasm.sh --check   # the committed CW decoder artifact (skips without emsdk)
	python3 tools/quality/architecture_guard.py
	./build.sh
	@echo "OK: in-process gates green (same as the backend/frontend/architecture jobs of .github/workflows/ci.yml; browser e2e: make e2e-fake)"

wasm:     ## Build both WASM cores (DSP + DFN) and publish the committed artifacts (needs Rust)
	./wasm/build.sh
	./wasm-dfn/build.sh

ggmorse:  ## Rebuild the CW decoder wasm module (needs emsdk; the artifact is committed)
	tools/build_ggmorse_wasm.sh

ggmorse-check: ## Check the committed CW decoder artifact against its expected API (no emsdk needed)
	tools/build_ggmorse_wasm.sh --check

wasm-check: ## Rebuild both WASM cores and fail if a committed artifact differs (needs Rust)
	./wasm/build.sh --check
	./wasm-dfn/build.sh --check

wasm-test: ## Run the Rust DSP kernel tests (numeric agreement with the Python reference)
	cd wasm && cargo test

frontend/modern/public/dsp.wasm: $(WASM_DSP_SRC)
	@if ! command -v cargo >/dev/null 2>&1; then \
		echo "NOTE: $@ is stale but cargo is missing - keeping the committed artifact" \
			"(install rustup, then: cargo install wasm-pack && rustup target add wasm32-unknown-unknown)"; \
	else rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true; ./wasm/build.sh; fi

frontend/modern/public/dfn/df_bg.wasm: $(WASM_DFN_SRC)
	@if ! command -v cargo >/dev/null 2>&1; then \
		echo "NOTE: $@ is stale but cargo is missing - keeping the committed artifact" \
			"(install rustup, then: cargo install wasm-pack && rustup target add wasm32-unknown-unknown)"; \
	else rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true; \
		command -v wasm-pack >/dev/null 2>&1 || cargo install wasm-pack; ./wasm-dfn/build.sh; fi

hw-test:  ## Hardware-in-the-loop: tinySA CW + SWP/RTA smoke + UI state regression (SAN-90 required)
	@# [w]eb_sa: the bracket keeps pgrep from matching this recipe's own shell wrapper
	@pgrep -f "[w]eb_sa.supervisor" >/dev/null || ./run.sh
	@# The smoke test needs std mode; a previous e2e run may have left SDR/RTA active.
	@curl -s -X POST http://127.0.0.1:$${WEBSA_PORT:-8080}/api/config \
		-H 'Content-Type: application/json' -d '{"cmd":"SET_MODE","mode":"std"}' >/dev/null || true
	python3 tools/hardware_smoke.py --tinysa-port $${TINYSA_PORT:-/dev/ttyACM0} \
		--configure-tinysa --frequency 100.2e6 --span 10e6 --duration 3
	python3 tools/command_sweep.py
	python3 tools/e2e/state_regression.py
	@echo "OK: hardware smoke + command sweep + UI state regression"

bench:    ## Performance baseline: compare against tools/bench_baseline.json (service running)
	@pgrep -f "[w]eb_sa.supervisor" >/dev/null || ./run.sh
	python3 tools/bench.py --check tools/bench_baseline.json

e2e-fake:  ## UI smoke against the fake backend (no hardware, no vendor library)
	@./stop.sh >/dev/null 2>&1 || true
	@WEBSA_FAKE=1 WEBSA_PORT=$${WEBSA_FAKE_PORT:-8099} setsid nohup python3 -m web_sa.supervisor \
		> /tmp/websa-fake.log 2>&1 < /dev/null & echo $$! > /tmp/websa-fake.pid
	@for i in $$(seq 1 25); do sleep 1; \
		curl -sf -o /dev/null http://127.0.0.1:$${WEBSA_FAKE_PORT:-8099}/api/state && break; done
	python3 tools/e2e/ui_smoke.py --url http://127.0.0.1:$${WEBSA_FAKE_PORT:-8099}
	python3 tools/e2e/state_regression.py --url http://127.0.0.1:$${WEBSA_FAKE_PORT:-8099}
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_FAKE_PORT:-8099} \
		--check --no-inventory
	@kill $$(cat /tmp/websa-fake.pid) 2>/dev/null || true; rm -f /tmp/websa-fake.pid
	@echo "OK: fake-backend UI smoke + state-machine contract + resolution sweep (no hardware)"

bench-record:  ## Re-record the performance baseline on this host
	python3 tools/bench.py --duration 4 --write-baseline tools/bench_baseline.json

viewport-baseline:  ## Layout/resolution baseline over every attached output (needs a running service)
	@pgrep -f "[w]eb_sa.supervisor" >/dev/null || ./run.sh
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_PORT:-8080} \
		--json tools/e2e/viewport_baseline.json

viewport-check:  ## Resolution/geometry gate: every viewport must come back clean (needs a running service)
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_PORT:-8080} \
		--check --no-inventory

all:      ## Build + test + run
	$(MAKE) build
	./test.sh
	./run.sh

help:     ## Show help
	@grep -E '^[a-zA-Z_-]+:.*##' Makefile | awk 'BEGIN {FS = ":.*## "}; {printf "  %-13s %s\n", $$1, $$2}'
