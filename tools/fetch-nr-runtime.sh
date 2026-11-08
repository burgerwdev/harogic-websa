#!/usr/bin/env bash
# Fetch the pinned native CPU library, without a Python dependency.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)/$(uname -m)" == Linux/x86_64 ]] || { echo 'Expected Linux x86_64' >&2; exit 1; }
task_dir="$(mktemp -d)"
trap 'rm -rf -- "$task_dir"' EXIT
curl -fL https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-linux-x64-1.23.2.tgz -o "$task_dir/runtime.tgz"
(cd "$task_dir" && echo '1fa4dcaef22f6f7d5cd81b28c2800414350c10116f5fdd46a2160082551c5f9b  runtime.tgz' | sha256sum -c -)
tar -xzf "$task_dir/runtime.tgz" -C "$task_dir"
mkdir -p runtime/linux-x86_64
install -m 755 "$task_dir/onnxruntime-linux-x64-1.23.2/lib/libonnxruntime.so.1.23.2" runtime/linux-x86_64/libonnxruntime.so.1.23.2
