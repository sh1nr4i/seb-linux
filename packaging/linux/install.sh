#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DEPS=1

for arg in "$@"; do
  case "${arg}" in
    --no-deps) INSTALL_DEPS=0 ;;
    *)
      echo "Usage: $0 [--no-deps]" >&2
      exit 1
      ;;
  esac
done

if [[ "${EUID}" -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1; then
    exec sudo "$0" "$@"
  elif command -v doas >/dev/null 2>&1; then
    exec doas "$0" "$@"
  elif command -v pkexec >/dev/null 2>&1; then
    exec pkexec "$0" "$@"
  else
    echo "Please run this installer as root." >&2
    exit 1
  fi
fi

has_shared_lib() {
  local lib="$1"
  if command -v ldconfig >/dev/null 2>&1 && ldconfig -p 2>/dev/null | grep -q "${lib}"; then
    return 0
  fi
  find /usr/lib /usr/lib64 /lib /lib64 -maxdepth 3 -name "${lib}*" 2>/dev/null | grep -q .
}

install_missing_runtime_deps() {
  local need_install=0
  if ! has_shared_lib "libwebkit2gtk-4.1.so.0" || ! has_shared_lib "libgtk-3.so.0"; then
    need_install=1
  fi
  if ! command -v pkexec >/dev/null 2>&1 && ! command -v doas >/dev/null 2>&1; then
    need_install=1
  fi
  if [[ "${need_install}" -eq 0 ]]; then
    echo "All required runtime dependencies are already installed."
    return 0
  fi

  echo "Installing missing system runtime dependencies (webkit2gtk-4.1, gtk3, polkit)..."
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y libwebkit2gtk-4.1-0 desktop-file-utils shared-mime-info || true
    apt-get install -y policykit-1 || apt-get install -y polkitd pkexec || true
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y webkit2gtk4.1 gtk3 polkit desktop-file-utils shared-mime-info
  elif command -v pacman >/dev/null 2>&1; then
    pacman -S --needed --noconfirm webkit2gtk-4.1 gtk3 polkit desktop-file-utils shared-mime-info
  elif command -v zypper >/dev/null 2>&1; then
    zypper --non-interactive install libwebkit2gtk-4_1-0 gtk3 polkit desktop-file-utils shared-mime-info
  elif command -v apk >/dev/null 2>&1; then
    apk add --no-cache webkit2gtk-4.1 gtk+3.0 polkit doas desktop-file-utils shared-mime-info
  else
    echo "Warning: unrecognized package manager. Install webkit2gtk-4.1, gtk3, and polkit manually." >&2
  fi
}

if [[ "${INSTALL_DEPS}" -eq 1 ]]; then
  install_missing_runtime_deps
fi

echo "Installing Safe Exam Browser binaries and Polkit policy..."
install -Dm755 "${SCRIPT_DIR}/bin/safe-exam-browser" "/usr/bin/safe-exam-browser"
install -Dm755 "${SCRIPT_DIR}/bin/safe-exam-browser-lockdown" "/usr/bin/safe-exam-browser-lockdown"
install -Dm755 "${SCRIPT_DIR}/bin/libseb_web_extension.so" "/usr/bin/libseb_web_extension.so"
install -Dm755 "${SCRIPT_DIR}/bin/libseb_web_extension.so" "/usr/lib/safe-exam-browser/libseb_web_extension.so"

install -Dm644 "${SCRIPT_DIR}/share/polkit-1/actions/org.safeexambrowser.lockdown.policy" \
  "/usr/share/polkit-1/actions/org.safeexambrowser.lockdown.policy"
install -Dm644 "${SCRIPT_DIR}/share/applications/safe-exam-browser.desktop" \
  "/usr/share/applications/safe-exam-browser.desktop"
install -Dm644 "${SCRIPT_DIR}/share/mime/packages/safe-exam-browser.xml" \
  "/usr/share/mime/packages/safe-exam-browser.xml"
install -Dm644 "${SCRIPT_DIR}/share/metainfo/safe-exam-browser.metainfo.xml" \
  "/usr/share/metainfo/safe-exam-browser.metainfo.xml"
install -Dm644 "${SCRIPT_DIR}/share/icons/hicolor/64x64/apps/safe-exam-browser.png" \
  "/usr/share/icons/hicolor/64x64/apps/safe-exam-browser.png"
install -Dm644 "${SCRIPT_DIR}/share/icons/hicolor/256x256/apps/safe-exam-browser.png" \
  "/usr/share/icons/hicolor/256x256/apps/safe-exam-browser.png"
install -Dm644 "${SCRIPT_DIR}/share/icons/hicolor/256x256/apps/safe-exam-browser.png" \
  "/usr/share/pixmaps/safe-exam-browser.png"

update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
update-mime-database /usr/share/mime >/dev/null 2>&1 || true
gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || true

echo "Safe Exam Browser installed successfully."
