#!/usr/bin/env bash
# Resolve the versioned CEF distribution directory for a build.
#
#   set-cef-path.sh [target-triple]
#
# With no argument the runner's own OS/architecture is used. With a triple the
# distribution for that triple is resolved instead, which is what the macOS
# universal build needs: it packages x86_64 and aarch64 separately and merges
# them, so each pass must point CEF_PATH at its own architecture's SDK.
#
# Exports (to GITHUB_ENV):
#   CEF_PATH                 resolved distribution for this invocation
#   CEF_VERSION_DIR          build/cef/<version>, useful for cache probes
#   CEF_PATH_MACOS_X86_64    both Apple distributions, exported on macOS runners
#   CEF_PATH_MACOS_AARCH64   regardless of the runner's own architecture
set -euo pipefail

# Must track CEF_SHORT_VERSION in crates/SphereWebView/src/lib.rs.
cef_version="150.0.11"

# Platform directory names, matching CefTarget::directory_name(). The official
# CEF index (https://cef-builds.spotifycdn.com/index.json) publishes these as
# windows64 / linux64 / macosx64 / macosarm64 — there is no universal macOS
# distribution, so a universal app is built by merging macosx64 + macosarm64.
platform_dir_for_triple() {
  case "$1" in
    x86_64-pc-windows-msvc)  echo "cef_windows_x86_64" ;;
    x86_64-unknown-linux-gnu) echo "cef_linux_x86_64" ;;
    x86_64-apple-darwin)     echo "cef_macos_x86_64" ;;
    aarch64-apple-darwin)    echo "cef_macos_aarch64" ;;
    *) return 1 ;;
  esac
}

triple_for_runner() {
  case "${RUNNER_OS:?RUNNER_OS is required}-${RUNNER_ARCH:?RUNNER_ARCH is required}" in
    Windows-X64) echo "x86_64-pc-windows-msvc" ;;
    Linux-X64)   echo "x86_64-unknown-linux-gnu" ;;
    macOS-X64)   echo "x86_64-apple-darwin" ;;
    macOS-ARM64) echo "aarch64-apple-darwin" ;;
    *) return 1 ;;
  esac
}

workspace="${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
version_dir="${workspace}/build/cef/${cef_version}"

cef_path_for_triple() {
  local platform
  platform="$(platform_dir_for_triple "$1")" || return 1
  echo "${version_dir}/${platform}"
}

if [[ $# -gt 0 ]]; then
  triple="$1"
else
  triple="$(triple_for_runner)" || {
    echo "Unsupported CEF runner: ${RUNNER_OS}-${RUNNER_ARCH}" >&2
    exit 1
  }
fi

cef_path="$(cef_path_for_triple "$triple")" || {
  echo "Unsupported CEF target triple: ${triple}" >&2
  exit 1
}

github_env="${GITHUB_ENV:?GITHUB_ENV is required}"
{
  echo "CEF_PATH=${cef_path}"
  echo "CEF_VERSION_DIR=${version_dir}"
} >> "$github_env"
echo "CEF_PATH=${cef_path} (${triple})"

# A universal macOS package needs both Apple SDKs staged on the same runner, so
# publish both paths up front instead of re-running this script per pass.
if [[ "$triple" == *apple-darwin ]]; then
  {
    echo "CEF_PATH_MACOS_X86_64=$(cef_path_for_triple x86_64-apple-darwin)"
    echo "CEF_PATH_MACOS_AARCH64=$(cef_path_for_triple aarch64-apple-darwin)"
  } >> "$github_env"
  echo "CEF_PATH_MACOS_X86_64=$(cef_path_for_triple x86_64-apple-darwin)"
  echo "CEF_PATH_MACOS_AARCH64=$(cef_path_for_triple aarch64-apple-darwin)"
fi
