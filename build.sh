#!/bin/bash
# Frontend build: npm install (if missing) -> Vite build -> dist
# Self-hosted font: extract LXGW WenKai from the system if public/fonts lacks LXGWWenKai-*.woff2
set -e
cd "$(dirname "$0")/frontend/modern"

# Font extraction (if missing): subset LXGW WenKai from /usr/share/fonts/TTF
if [ ! -f public/fonts/LXGWWenKai-Regular.woff2 ]; then
  echo "Extracting self-hosted font (LXGW WenKai subset)..."
  python3 - <<'PY'
import subprocess, os, sys
base = '/usr/share/fonts/TTF'
out = 'public/fonts'
os.makedirs(out, exist_ok=True)
# Build GB2312 character list
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
for w in ('Regular', 'Medium'):
    ttf = f'{base}/LXGWWenKai-{w}.ttf'
    if not os.path.exists(ttf):
        print(f'WARN: {ttf} not found, skipping {w} font extraction'); continue
    subprocess.run([sys.executable, '-m', 'fontTools.subset', ttf,
                    f'--text-file={tmp}', f'--unicodes={unicodes}',
                    '--flavor=woff2', f'--output-file={out}/LXGWWenKai-{w}.woff2'],
                   check=True, capture_output=True)
    print(f'  font: LXGWWenKai-{w}.woff2')
PY
fi

# npm deps + build
if [ ! -d node_modules ]; then
  echo "Installing npm dependencies..."
  npm install
fi
npm run build
echo "OK: frontend build complete -> frontend/modern/dist"
