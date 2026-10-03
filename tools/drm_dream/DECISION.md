# Decision: which `dream` binary the DRM mode runs

Status: **accepted** (user-approved during the goal's clarification round).

## Context

The goal named the installed `/usr/bin/dream` ("dream-nox 2.2.4") as the background decoder and
requires station metadata (label / robustness mode / bitrate) plus decoded AAC audio.

## Finding

Neither installed build exposes Dream's `--status-socket` JSON status stream, which is how the
metadata is obtained:

| Installed binary | Build | `--status-socket` |
| --- | --- | --- |
| `dream-nox` 2.2.4 (wwek fork) | `qmake CONFIG+=qtconsole` | parsed but **not compiled in** — no `StatusBroadcast` code |
| `dream` 2.3 (upstream `Drm-tools/dream`) | CMake `USE_QT=ON` | **absent** (only `DreamLog.txt` / `DreamLogLong.csv`) |

Evidence: `strings /usr/bin/dream | grep -i statusbroadcast` is empty, and a run with
`--status-socket` never creates the socket. The same wwek 2.2.4 source built with the project's
documented headless mode (`qmake CONFIG+=console`, which selects `src/main.cpp` and starts
`CStatusBroadcast`) does provide it.

## Decision

The user was asked, with the finding above, and chose **"Vendor wwek console build
(Recommended)"**: build the same wwek `v2.2.4` source with `CONFIG+=console` into the worktree and
run that as the background decoder. The installed binaries are left untouched.

## How the code honours the original request

`web_sa/measurements/sdr.py::_default_dream_bin()` prefers, in order:

1. `DRM_DREAM_BIN` (explicit override),
2. `/usr/local/bin/dream`,
3. `/usr/bin/dream`,
4. the vendored build `tools/drm_dream/build/dream`,

selecting the first candidate whose `dream --help` advertises `--status-socket`. So an installed
`/usr/bin/dream` is used automatically as soon as the packaged build gains the feature — the
vendored build is a capability fallback, not a hard-coded replacement. To use the installed binary
here, replace the package with the console build or point `DRM_DREAM_BIN` at one.

Build the fallback with `tools/drm_dream/build_dream.sh` (git-ignored output).
