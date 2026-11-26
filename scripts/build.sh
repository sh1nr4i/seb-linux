#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_BIN_DIR="${ROOT_DIR}/build/bin"
DEV_BUILD=0

for arg in "$@"; do
  case "${arg}" in
    --dev|-DSEB_DEV=ON)
      DEV_BUILD=1
      ;;
    --user|-DSEB_DEV=OFF)
      DEV_BUILD=0
      ;;
    *)
      echo "Unknown build argument: ${arg}" >&2
      echo "Usage: $0 [--dev | -DSEB_DEV=ON | --user | -DSEB_DEV=OFF]" >&2
      exit 1
      ;;
  esac
done

cd "${ROOT_DIR}"

CARGO_ARGS=(build --release -p seb-app -p seb-lockdown-helper -p seb-web-extension)
if [[ "${DEV_BUILD}" -eq 1 ]]; then
  echo "Building Safe Exam Browser (Rust, Dev Build: --features seb-app/dev)..."
  CARGO_ARGS+=(--features seb-app/dev)
else
  echo "Building Safe Exam Browser (Rust, User Build: strict anti-cheat default)..."
fi

cargo "${CARGO_ARGS[@]}"

mkdir -p "${BUILD_BIN_DIR}"
cp -f "${ROOT_DIR}/target/release/safe-exam-browser" "${BUILD_BIN_DIR}/safe-exam-browser"
cp -f "${ROOT_DIR}/target/release/safe-exam-browser-lockdown" "${BUILD_BIN_DIR}/safe-exam-browser-lockdown"
cp -f "${ROOT_DIR}/target/release/libseb_web_extension.so" "${BUILD_BIN_DIR}/libseb_web_extension.so"

echo "Build outputs:"
echo "  ${BUILD_BIN_DIR}/safe-exam-browser"
echo "  ${BUILD_BIN_DIR}/safe-exam-browser-lockdown"
echo "  ${BUILD_BIN_DIR}/libseb_web_extension.so"
