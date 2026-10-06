# Unified entry, organised by functional area (Service / Build / Test / WASM artifacts /
# Hardware / E2E / Misc). Run `make help` for the grouped list.
#
# Build strategy: the WASM cores are COMMITTED (frontend/public/dsp.wasm, dfn/df_bg.wasm), so a
# machine without Rust still builds. `make build` re-runs a WASM core only when a source is newer
# than the published artifact AND a toolchain exists. `make clean` keeps node_modules and the cargo
# target dirs, so `make clean && make build` is incremental; `make clean-all` is the full wipe.

.PHONY: run stop restart status \
	build frontend wasm wasm-dsp wasm-dfn dev clean clean-all \
	test wasm-test ci wasm-check ggmorse ggmorse-check \
	hw-test bench bench-record e2e-fake viewport-baseline viewport-check \
	all help

# WASM artifact staleness: the artifacts are COMMITTED (the service and CI gate on them), so a
# machine without Rust still builds fine - make only re-runs the wasm build when a source is
# newer than the published artifact AND a toolchain exists.
WASM_DSP_SRC := $(shell find wasm/src -name '*.rs' 2>/dev/null) wasm/Cargo.toml wasm/build.sh
WASM_DFN_SRC := $(shell find wasm-dfn/src -name '*.rs' 2>/dev/null) wasm-dfn/Cargo.toml wasm-dfn/build.sh

##@ Service

run:      ## Start service
	./run.sh

stop:     ## Stop service
	./stop.sh

restart:  ## Restart service (stop + start)
	./stop.sh
	./run.sh

status:   ## Show service status (pid, uptime, memory, CPU, log path/size)
	./status.sh

##@ Build

frontend: ## Frontend only: npm install + Vite build -> dist (no WASM)
	./build.sh

wasm-dsp: ## Build the DSP core (needs Rust) and publish the committed artifact
	./wasm/build.sh

wasm-dfn: ## Build the DFN (DeepFilterNet3) core (needs Rust) and publish the committed artifact
	./wasm-dfn/build.sh

wasm: wasm-dsp wasm-dfn  ## Build both WASM cores (DSP + DFN) and publish the committed artifacts (needs Rust)

build: frontend/public/dsp.wasm frontend/public/dfn/df_bg.wasm  ## Full build: WASM cores (only if stale + Rust present) + frontend -> dist
	./build.sh

clean:    ## Clean caches/logs/dist, keeping deps + WASM caches (fast rebuild)
	./clean.sh --keep-deps

clean-all: ## Clean everything, including node_modules and the WASM target dirs (full rebuild)
	./clean.sh

dev:      ## Stop, clean (deps kept), rebuild frontend, run with WEBSA_TRACE=1
	./stop.sh
	$(MAKE) clean
	$(MAKE) build
	WEBSA_TRACE=1 ./run.sh
	@echo "trace log: /tmp/websa.log  (tail -f /tmp/websa.log)"

##@ Test

test:     ## Test (backend pytest + frontend vitest)
	./test.sh

wasm-test: ## Run the Rust DSP kernel tests (numeric agreement with the Python reference)
	cd wasm && cargo test

ci:       ## The in-process gates CI runs, locally (no hardware; browser e2e: make e2e-fake)
	./test.sh
	python3 tools/checks/sync_version.py --check
	python3 tools/fixtures/gen_frame_fixtures.py --check
	python3 tools/fixtures/gen_dsp_fixtures.py --check
	python3 tools/fixtures/gen_ft8_fixtures.py --check
	python3 tools/checks/check_dom_ids.py
	python3 tools/checks/check_registrations.py
	python3 tools/checks/check_docs_parity.py
	python3 tools/checks/check_doc_paths.py
	python3 tools/checks/check_wasm_artifact.py
	tools/vendor/build_ggmorse_wasm.sh --check   # the committed CW decoder artifact (skips without emsdk)
	python3 tools/checks/architecture_guard.py
	./build.sh
	@echo "OK: in-process gates green (same as the backend/frontend/architecture jobs of .github/workflows/ci.yml; browser e2e: make e2e-fake)"

##@ WASM artifacts

wasm-check: ## Rebuild both WASM cores and fail if a committed artifact differs (needs Rust)
	./wasm/build.sh --check
	./wasm-dfn/build.sh --check

ggmorse:  ## Rebuild the CW decoder wasm module (needs emsdk; the artifact is committed)
	tools/vendor/build_ggmorse_wasm.sh

ggmorse-check: ## Check the committed CW decoder artifact against its expected API (no emsdk needed)
	tools/vendor/build_ggmorse_wasm.sh --check

##@ Hardware

hw-test:  ## Hardware-in-the-loop: tinySA CW + SWP/RTA smoke + UI state regression (SAN-90 required)
	@# [w]eb_sa: the bracket keeps pgrep from matching this recipe's own shell wrapper
	@pgrep -f "[w]eb_sa.supervisor" >/dev/null || ./run.sh
	@# The smoke test needs std mode; a previous e2e run may have left SDR/RTA active.
	@curl -s -X POST http://127.0.0.1:$${WEBSA_PORT:-8080}/api/config \
		-H 'Content-Type: application/json' -d '{"cmd":"SET_MODE","mode":"std"}' >/dev/null || true
	python3 tools/bench/hardware_smoke.py --tinysa-port $${TINYSA_PORT:-/dev/ttyACM0} \
		--configure-tinysa --frequency 100.2e6 --span 10e6 --duration 3
	python3 tools/bench/command_sweep.py
	python3 tools/e2e/state_regression.py
	@echo "OK: hardware smoke + command sweep + UI state regression"

bench:    ## Performance baseline: compare against tools/bench/bench_baseline.json (service running)
	@pgrep -f "[w]eb_sa.supervisor" >/dev/null || ./run.sh
	python3 tools/bench/bench.py --check tools/bench/bench_baseline.json

bench-record:  ## Re-record the performance baseline on this host
	python3 tools/bench/bench.py --duration 4 --write-baseline tools/bench/bench_baseline.json

##@ E2E

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

viewport-baseline:  ## Layout/resolution baseline over every attached output (needs a running service)
	@pgrep -f "[w]eb_sa.supervisor" >/dev/null || ./run.sh
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_PORT:-8080} \
		--json tools/e2e/viewport_baseline.json

viewport-check:  ## Resolution/geometry gate: every viewport must come back clean (needs a running service)
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_PORT:-8080} \
		--check --no-inventory

##@ Misc

all:      ## Build + test + run
	$(MAKE) build
	./test.sh
	./run.sh

help:     ## Show help
	@awk 'BEGIN {FS = ":.*##"; print "Usage: make <target>\n"} \
		/^##@/ {printf "\n%s\n", substr($$0, 5)} \
		/^[a-zA-Z0-9_-]+:.*##/ {printf "  %-18s %s\n", $$1, $$2}' Makefile

# Committed WASM artifacts (not listed in help): rebuild only when sources are newer AND a
# toolchain exists. They are the staleness gate `make build` keys on.
frontend/public/dsp.wasm: $(WASM_DSP_SRC)
	@if ! command -v cargo >/dev/null 2>&1; then \
		echo "NOTE: $@ is stale but cargo is missing - keeping the committed artifact" \
			"(install rustup, then: cargo install wasm-pack && rustup target add wasm32-unknown-unknown)"; \
	else rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true; ./wasm/build.sh; fi

frontend/public/dfn/df_bg.wasm: $(WASM_DFN_SRC)
	@if ! command -v cargo >/dev/null 2>&1; then \
		echo "NOTE: $@ is stale but cargo is missing - keeping the committed artifact" \
			"(install rustup, then: cargo install wasm-pack && rustup target add wasm32-unknown-unknown)"; \
	else rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true; \
		command -v wasm-pack >/dev/null 2>&1 || cargo install wasm-pack; ./wasm-dfn/build.sh; fi
