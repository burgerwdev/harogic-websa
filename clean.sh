#!/bin/bash
# 清理: 缓存/日志/构建产物(保留源码)
cd "$(dirname "$0")"
rm -rf web_sa/**/__pycache__ web_sa/**/**/__pycache__ 2>/dev/null
find . -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null
rm -rf .pytest_cache .ruff_cache
rm -f /tmp/san90-web.log
rm -rf frontend/modern/dist
rm -rf frontend/modern/node_modules
echo "OK: 已清理 (缓存/日志/构建产物; 运行 ./build.sh 重建前端)"
