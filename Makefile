# Unified entry
.PHONY: run stop clean build test all

run:      ## Start service
	./run.sh

stop:     ## Stop service
	./stop.sh

clean:    ## Clean caches/logs/artifacts
	./clean.sh

build:    ## Frontend build
	./build.sh

test:     ## Test (backend pytest + frontend vitest)
	./test.sh

all:      ## Build + test + run
	./build.sh && ./test.sh && ./run.sh

help:     ## Show help
	@grep -E '^[a-zA-Z_-]+:.*##' Makefile | awk 'BEGIN {FS = ":.*## "}; {printf "  %-8s %s\n", $$1, $$2}'
