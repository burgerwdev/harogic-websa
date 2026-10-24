# Build the browser controls and native SDR service separately.
# WASM targets remain explicit offline DSP diagnostics.

.PHONY: run stop restart status \
	build frontend native sdk-shim package-runtime wasm wasm-dsp wasm-dfn dev clean clean-all \
	test wasm-test ci wasm-check \
	hw-test bench bench-record e2e-fake viewport-baseline viewport-check \
	all help

# Explicit diagnostic artifact dependencies.
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

status:   ## Show Rust user-unit status, journal and HTTP health
	./status.sh

##@ Build

frontend: ## Frontend only: npm install + Vite build -> dist (no WASM)
	./build.sh

wasm-dsp: ## Build the DSP core (needs Rust) and publish the committed artifact
	./wasm/build.sh

wasm-dfn: ## Build the DFN (DeepFilterNet3) core (needs Rust) and publish the committed artifact
	./wasm-dfn/build.sh

wasm: wasm-dsp wasm-dfn  ## Build both WASM cores (DSP + DFN) and publish the committed artifacts (needs Rust)

build: frontend native  ## Build browser controls and Rust service

native: ## Build the Rust service and native DSP
	cargo build -j 1 --release --manifest-path native-ddc/Cargo.toml --bin websad

SDK_ROOT ?= /opt/htraapi
SDK_SHIM ?= $(HOME)/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so
sdk-shim: ## Build matching SAN-90 retained-session SDK shim (vendor SDK required)
	mkdir -p "$(dir $(SDK_SHIM))"
	$(CXX) -O2 -std=c++17 -Wall -Wextra -pthread -fPIC -shared -I"$(SDK_ROOT)/inc" native-ddc/sdk/san90_sdk.cpp -L"$(SDK_ROOT)/lib/x86_64" -Wl,-rpath,"$(SDK_ROOT)/lib/x86_64" -l:libhtraapi.so.0.55.100 -o "$(SDK_SHIM)"

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
	@echo "Rust service journal: journalctl --user -u $${WEBSA_UNIT:-websad} -f"

##@ Test

test:     ## Test Rust service contracts, Python tools and frontend
	./test.sh

wasm-test: ## Run native Rust DSP kernel tests
	cargo test -j 1 --manifest-path wasm/Cargo.toml -- --test-threads=1

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
	./wasm/build.sh --check
	./wasm-dfn/build.sh --check

##@ Hardware

hw-test:  ## Hardware-in-the-loop: tinySA CW + SWP/RTA smoke + UI state regression (SAN-90 required)
	@systemctl --user is-active --quiet "$${WEBSA_UNIT:-websad}" || ./run.sh
	@# The smoke test needs std mode; a previous e2e run may have left SDR/RTA active.
	@curl -s -X POST http://127.0.0.1:$${WEBSA_PORT:-8080}/api/config \
		-H 'Content-Type: application/json' -d '{"cmd":"SET_MODE","mode":"std"}' >/dev/null || true
	python3 tools/bench/hardware_smoke.py --tinysa-port $${TINYSA_PORT:-/dev/ttyACM0} \
		--configure-tinysa --frequency 100.2e6 --span 10e6 --duration 3
	python3 tools/bench/command_sweep.py
	python3 tools/e2e/state_regression.py
	@echo "OK: hardware smoke + command sweep + UI state regression"

bench:    ## Performance baseline: compare against tools/bench/bench_baseline.json (service running)
	@systemctl --user is-active --quiet "$${WEBSA_UNIT:-websad}" || ./run.sh
	python3 tools/bench/bench.py --check tools/bench/bench_baseline.json

bench-record:  ## Re-record the performance baseline on this host
	python3 tools/bench/bench.py --duration 4 --write-baseline tools/bench/bench_baseline.json

##@ E2E

e2e-fake: native  ## UI smoke against the native fixture service; production remains unchanged
	@bash -eu -o pipefail -c '\
	WEBSA_FAKE_PORT=$${WEBSA_FAKE_PORT:-8099}; \
	export WEBSA_PORT=$$WEBSA_FAKE_PORT; \
	env -u WEBSA_RUST_IQS_SHIM WEBSA_HOST=127.0.0.1 setsid nohup native-ddc/target/release/websad > /tmp/websa-fake.log 2>&1 < /dev/null & \
	fake_pid=$$!; echo $$fake_pid > /tmp/websa-fake.pid; \
	cleanup() { kill -TERM -- -$$fake_pid 2>/dev/null || kill "$$fake_pid" 2>/dev/null || true; rm -f /tmp/websa-fake.pid; }; \
	trap cleanup EXIT; \
	ok=0; \
	for i in $$(seq 1 25); do sleep 1; \
		curl -sf -o /dev/null http://127.0.0.1:$$WEBSA_FAKE_PORT/api/state && { ok=1; break; }; done; \
	if [ $$ok -ne 1 ]; then echo "FAIL: fake backend did not become ready" >&2; tail -5 /tmp/websa-fake.log >&2; exit 1; fi; \
	python3 tools/e2e/ui_smoke.py --url http://127.0.0.1:$$WEBSA_FAKE_PORT; \
	python3 tools/e2e/state_regression.py --url http://127.0.0.1:$$WEBSA_FAKE_PORT; \
	python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:$$WEBSA_FAKE_PORT \
		--check --no-inventory; \
	echo "OK: fake-backend UI smoke + state-machine contract + resolution sweep (no hardware)"'

VIEWPORT_JSON ?= /tmp/websa-viewport.json

viewport-baseline:  ## Record current layout/geometry findings over attached outputs (VIEWPORT_JSON sets output)
	@systemctl --user is-active --quiet "$${WEBSA_UNIT:-websad}" || ./run.sh
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
	./wasm/build.sh

frontend/public/dfn/df_bg.wasm: $(WASM_DFN_SRC)
	./wasm-dfn/build.sh
