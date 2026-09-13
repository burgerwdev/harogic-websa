# Unified entry
.PHONY: run stop clean build test dev all help

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

all:      ## Build + test + run
	./build.sh && ./test.sh && ./run.sh

help:     ## Show help
	@grep -E '^[a-zA-Z_-]+:.*##' Makefile | awk 'BEGIN {FS = ":.*## "}; {printf "  %-8s %s\n", $$1, $$2}'
