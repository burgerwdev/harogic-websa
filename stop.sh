#!/bin/bash
# 停止服务
cd "$(dirname "$0")"
pkill -9 -f "python3 -m web_sa.main" 2>/dev/null
sleep 1
if pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1; then
  echo "未能完全停止(残留进程: $(pgrep -f 'python3 -m web_sa.main' | tr '\n' ' '))"
  exit 1
fi
echo "OK: 服务已停止"
