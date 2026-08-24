#!/bin/bash
# 测试: 后端 pytest + 前端 vitest(如已装依赖)
set -e
cd "$(dirname "$0")"
echo "==== 后端测试 (pytest) ===="
python3 -m pytest tests/ -v 2>&1 | tail -15

echo
echo "==== 前端测试 (vitest) ===="
if [ -d frontend/modern/node_modules ]; then
  (cd frontend/modern && npx vitest run 2>&1 | tail -12)
else
  echo "前端依赖未安装, 跳过 (cd frontend/modern && npm install 后重跑)"
fi
echo "OK: 测试完成"
