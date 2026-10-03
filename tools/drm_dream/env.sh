#!/usr/bin/env bash
# Isolation conventions for the DRM-via-dream worktree.
#
# This worktree (feature/drm-dream-decoder) runs a *background* upstream Dream
# decoder. It must never collide with the other pi worktree
# (/home/hui/git/harogic-websa, branch feature/drm-demod) which owns
# WEBSA_PORT=8080 / WEBSA_FAKE_PORT=8099.
#
# Source this file before launching the service or the e2e tests:
#   source tools/drm_dream/env.sh
set -a

# Web service / e2e ports (deliberately offset from 8080 / 8099).
export WEBSA_PORT="${WEBSA_PORT:-8180}"
export WEBSA_FAKE_PORT="${WEBSA_FAKE_PORT:-8199}"

# Dream subprocess IPC. Everything is namespaced so a stray process from the
# other worktree can never be picked up or clobbered.
export DRM_DREAM_STATUS_SOCKET="${DRM_DREAM_STATUS_SOCKET:-/tmp/drm-dream-status.sock}"
export DRM_DREAM_FEED_SINK="${DRM_DREAM_FEED_SINK:-drm_dream_feed}"
export DRM_DREAM_AUDIO_PATH="${DRM_DREAM_AUDIO_PATH:-/tmp/drm-dream-audio.wav}"
export DRM_DREAM_WORKDIR="${DRM_DREAM_WORKDIR:-/tmp/drm-dream-run}"

# The vendored console build of wwek/dream (see build_dream.sh). The packaged
# /usr/bin/dream (both dream-nox/qtconsole and upstream 2.3/GUI) has no
# --status-socket, so it cannot provide station metadata.
_DRM_DREAM_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export DRM_DREAM_BIN="${DRM_DREAM_BIN:-$_DRM_DREAM_HERE/build/dream}"

set +a
