#!/bin/bash
# 运行: 单次启动(不循环重试) — 清理残留 → 启动 → 健康检查(就绪即退出)
# 用法: ./run.sh  (modern TS 前端)
set -e
cd "$(dirname "$0")"

# 前端就绪检查: 需要 dist 存在
if [ ! -f frontend/modern/dist/index.html ]; then
  echo "前端未构建, 先执行 ./build.sh"
  exit 1
fi

pkill -9 -f "python3 -m web_sa.main" 2>/dev/null || true
sleep 2
setsid nohup python3 -m web_sa.main > /tmp/san90-web.log 2>&1 < /dev/null &
for i in $(seq 1 25); do
  sleep 1
  if curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:8080/api/state 2>/dev/null | grep -q 200; then
    echo "OK: 服务已就绪 (http://localhost:8080)"
    exit 0
  fi
done
echo "FAIL: 服务未就绪(单次启动, 不重试)"
tail -5 /tmp/san90-web.log
exit 1
