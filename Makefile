# Build the browser controls and native SDR service separately.
# WASM targets remain explicit offline DSP diagnostics.

.PHONY: run stop restart status \
	build frontend native sdk-shim package-runtime wasm wasm-dsp wasm-dfn dev clean clean-all \
	test wasm-test ci wasm-check \
	hw-test bench bench-record e2e-fake viewport-baseline viewport-check \
	all help

# Explicit diagnostic artifact dependencies.
WASM_DSP_SRC := $(shell find dsp/core/src -name '*.rs' 2>/dev/null) dsp/core/Cargo.toml dsp/core/build.sh
WASM_DFN_SRC := $(shell find dsp/deepfilter/src -name '*.rs' 2>/dev/null) dsp/deepfilter/Cargo.toml dsp/deepfilter/build.sh

##@ Service

run:      ## Start service
	./run.sh

stop:     ## Stop service
	./stop.sh

restart:  ## Restart service (stop + start)
	./stop.sh
	./run.sh

status:   ## Show owned Rust process, log path and HTTP health
	./status.sh

##@ Build

frontend: ## Frontend only: npm install + Vite build -> dist (no WASM)
	./build.sh

wasm-dsp: ## Build the DSP core (needs Rust) and publish the committed artifact
	./dsp/core/build.sh

wasm-dfn: ## Build the DFN (DeepFilterNet3) core (needs Rust) and publish the committed artifact
	./dsp/deepfilter/build.sh

wasm: wasm-dsp wasm-dfn  ## Build both WASM cores (DSP + DFN) and publish the committed artifacts (needs Rust)

build: frontend native  ## Build browser controls and Rust service

native: ## Build the Rust service and native DSP
	cargo build -j 1 --release --manifest-path backend/Cargo.toml --bin websad

SDK_ROOT ?= /opt/htraapi
SDK_SHIM ?= $(HOME)/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so
sdk-shim: ## Build matching SAN-90 retained-session SDK shim (vendor SDK required)
	mkdir -p "$(dir $(SDK_SHIM))"
	$(CXX) -O2 -std=c++17 -Wall -Wextra -pthread -fPIC -shared -I"$(SDK_ROOT)/inc" backend/sdk/san90_sdk.cpp -L"$(SDK_ROOT)/lib/x86_64" -Wl,-rpath,"$(SDK_ROOT)/lib/x86_64" -l:libhtraapi.so.0.55.100 -o "$(SDK_SHIM)"

package-runtime: ## Publish existing native/frontend/SDK-adapter builds into runtime/ (Linux x86_64)
	./tools/package-runtime.sh "$(SDK_SHIM)"

clean:    ## Clean caches/dist, keeping dependencies and Rust targets
	./clean.sh --keep-deps

clean-all: ## Clean dependencies and Rust targets; keep committed WASM cores
	./clean.sh

dev:      ## Stop, clean caches, rebuild browser and Rust service, then run
	./stop.sh
	$(MAKE) clean
	$(MAKE) build
	WEBSA_TRACE=1 ./run.sh
	@echo "Run ./status.sh to find the Rust service log path"

##@ Test

test:     ## Test Rust service contracts, Python tools and frontend
	./test.sh

wasm-test: ## Run native Rust DSP kernel tests
	cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml -- --test-threads=1

ci: wasm-test  ## Run hardware-free local checks; browser checks use make e2e-fake
	./test.sh
	python3 tools/checks/sync_version.py --check
	python3 tools/fixtures/gen_frame_fixtures.py --check
	python3 tools/fixtures/gen_dsp_fixtures.py --check
	python3 tools/fixtures/gen_ft8_fixtures.py --check
	python3 tools/fixtures/port_sstv_modes.py --check
	python3 tools/fixtures/gen_sstv_fixtures.py --check
	python3 tools/checks/check_dom_ids.py
	python3 tools/checks/check_registrations.py
	python3 tools/checks/check_docs_parity.py
	python3 tools/checks/check_doc_paths.py
	python3 tools/checks/architecture_guard.py
	./build.sh
	@echo "OK: local tests, fixture contracts, static gates and frontend build; browser checks: make e2e-fake"

##@ WASM artifacts

wasm-check: ## Rebuild both WASM cores and fail if a committed artifact differs (needs Rust)
	./dsp/core/build.sh --check
	./dsp/deepfilter/build.sh --check

##@ Hardware

hw-test:  ## Hardware-in-the-loop: tinySA CW + SWP/RTA smoke + UI state regression (SAN-90 required)
	@./status.sh --quiet || ./run.sh
	@# The smoke test needs std mode; a previous e2e run may have left SDR/RTA active.
	@curl -s -X POST http://127.0.0.1:$${WEBSA_PORT:-8080}/api/config \
		-H 'Content-Type: application/json' -d '{"cmd":"SET_MODE","mode":"std"}' >/dev/null || true
	python3 tools/bench/hardware_smoke.py --tinysa-port $${TINYSA_PORT:-/dev/ttyACM0} \
		--configure-tinysa --frequency 100.2e6 --span 10e6 --duration 3 \
		--http-url http://127.0.0.1:$${WEBSA_PORT:-8080} --ws-url ws://127.0.0.1:$${WEBSA_PORT:-8080}
	python3 tools/bench/command_sweep.py --http-url http://127.0.0.1:$${WEBSA_PORT:-8080}
	python3 tools/e2e/state_regression.py --url http://127.0.0.1:$${WEBSA_PORT:-8080}
	@echo "OK: hardware smoke + command sweep + UI state regression"

bench:    ## Performance baseline: compare against tools/bench/bench_baseline.json (service running)
	@./status.sh --quiet || ./run.sh
	python3 tools/bench/bench.py --check tools/bench/bench_baseline.json \
		--http-url http://127.0.0.1:$${WEBSA_PORT:-8080} --ws-url ws://127.0.0.1:$${WEBSA_PORT:-8080}

bench-record:  ## Re-record the performance baseline on this host
	python3 tools/bench/bench.py --duration 4 --write-baseline tools/bench/bench_baseline.json \
		--http-url http://127.0.0.1:$${WEBSA_PORT:-8080} --ws-url ws://127.0.0.1:$${WEBSA_PORT:-8080}

##@ E2E

e2e-fake: native  ## UI smoke against the native fixture service; production remains unchanged
	@bash -eu -o pipefail -c '\
	WEBSA_FAKE_PORT=$${WEBSA_FAKE_PORT:-8099}; \
	export WEBSA_PORT=$$WEBSA_FAKE_PORT; \
	export WEBSA_INSTANCE=websad-e2e; \
	cleanup() { ./stop.sh; }; \
	./run.sh --fake --host 127.0.0.1; \
	trap cleanup EXIT; \
	python3 tools/e2e/ui_smoke.py --url http://127.0.0.1:$$WEBSA_FAKE_PORT; \
	python3 tools/e2e/state_regression.py --url http://127.0.0.1:$$WEBSA_FAKE_PORT; \
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$$WEBSA_FAKE_PORT \
		--check --no-inventory; \
	echo "OK: fake-backend UI smoke + state-machine contract + resolution sweep (no hardware)"'

VIEWPORT_JSON ?= /tmp/websa-viewport.json

viewport-baseline:  ## Record current layout/geometry findings over attached outputs (VIEWPORT_JSON sets output)
	@./status.sh --quiet || ./run.sh
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_PORT:-8080} \
		--json "$(VIEWPORT_JSON)"

viewport-check:  ## Resolution/geometry gate: every viewport must come back clean (needs a running service)
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$${WEBSA_PORT:-8080} \
		--check --no-inventory

##@ Misc

all:      ## Build + test + run
	$(MAKE) build
	$(MAKE) wasm-test
	./test.sh
	./run.sh

help:     ## Show help
	@awk 'BEGIN {FS = ":.*##"; print "Usage: make <target>\n"} \
		/^##@/ {printf "\n%s\n", substr($$0, 5)} \
		/^[a-zA-Z0-9_-]+:.*##/ {printf "  %-18s %s\n", $$1, $$2}' Makefile

# Explicit offline diagnostic artifacts; product frontend builds do not use them.
frontend/public/dsp.wasm: $(WASM_DSP_SRC)
	./dsp/core/build.sh

frontend/public/dfn/df_bg.wasm: $(WASM_DFN_SRC)
	./dsp/deepfilter/build.sh
