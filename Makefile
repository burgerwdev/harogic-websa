# 统一入口
.PHONY: run stop clean build test all

run:      ## 启动服务 (WEB_SA_UI=modern 切新前端)
	./run.sh

stop:     ## 停止服务
	./stop.sh

clean:    ## 清理缓存/日志/构建产物
	./clean.sh

build:    ## 前端构建
	./build.sh

test:     ## 测试 (后端 pytest + 前端 vitest)
	./test.sh

all:      ## 构建 + 测试 + 启动
	./build.sh && ./test.sh && ./run.sh

help:     ## 显示帮助
	@grep -E '^[a-zA-Z_-]+:.*##' Makefile | awk 'BEGIN {FS = ":.*## "}; {printf "  %-8s %s\n", $$1, $$2}'
