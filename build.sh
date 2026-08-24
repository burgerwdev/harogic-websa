#!/bin/bash
# 前端构建: npm install(如缺) → Vite build → dist
# 自托管字体: 如 public/fonts 缺 WebSASCJK-*.woff2, 从系统 Noto Sans CJK 自动提取
set -e
cd "$(dirname "$0")/frontend/modern"

# 字体提取(如缺失): 从系统 /usr/share/fonts/noto-cjk 提取 SC 子集
if [ ! -f public/fonts/WebSASCJK-Regular.woff2 ]; then
  echo "提取自托管字体 (Noto Sans CJK SC 子集)..."
  python3 - <<'PY'
import subprocess, os, sys
base = '/usr/share/fonts/noto-cjk'
out = 'public/fonts'
os.makedirs(out, exist_ok=True)
# 生成 GB2312 字符清单
chars = set()
for hi in range(0xA1, 0xF8):
    for lo in range(0xA1, 0xFF):
        try:
            chars.add(bytes([hi, lo]).decode('gb2312'))
        except Exception:
            pass
chars.update("◀▶▲▼Δ≈×±→←↑↓©™℃①②③④⑤⑥⑦⑧⑨⑩✓✔✗◆●■★☆·…—«»＃＆＊＋，－．／：；＝？＠＿┌┐└┘├┤┬┴┼─│═║╔╗╚╝╠╣╦╩╬≡≦≧≠∴∵∶∷√⊙⊕⊖⊗∈∉")
tmp = '/tmp/cjk_chars.txt'
open(tmp, 'w', encoding='utf-8').write(''.join(sorted(chars)))
unicodes = "U+0020-007E,U+00A0-00FF,U+0100-017F,U+0391-03C9,U+2000-206F,U+2010-2027,U+2030-205E,U+20AC,U+2190-21FF,U+2500-25FF,U+25A0-25FF,U+27A1,U+2B05-2B07,U+00D7,U+00F7,U+00B1,U+00B0,U+2014,U+2026,U+3001-3002,U+3008-300B,U+300C-300F,U+3010-3011,U+FF01-FF5E"
for w in ('Regular', 'Bold'):
    ttc = f'{base}/NotoSansCJK-{w}.ttc'
    if not os.path.exists(ttc):
        print(f'警告: 未找到 {ttc}, 跳过 {w} 字体提取'); continue
    subprocess.run([sys.executable, '-m', 'fontTools.subset', ttc, '--font-number=2',
                    f'--text-file={tmp}', f'--unicodes={unicodes}',
                    '--flavor=woff2', f'--output-file={out}/WebSASCJK-{w}.woff2'],
                   check=True, capture_output=True)
    print(f'  字体: WebSASCJK-{w}.woff2')
PY
fi

# npm 依赖 + 构建
if [ ! -d node_modules ]; then
  echo "安装 npm 依赖..."
  npm install
fi
npm run build
echo "OK: 前端构建完成 -> frontend/modern/dist"
