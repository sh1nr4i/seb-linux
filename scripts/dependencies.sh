#!/usr/bin/env bash
set -euo pipefail

MODE="runtime"
for arg in "$@"; do
  case "${arg}" in
    --runtime) MODE="runtime" ;;
    --build|--dev) MODE="build" ;;
    *)
      echo "Usage: $0 [--runtime | --build]" >&2
      exit 1
      ;;
  esac
done

SUDO=""
if [[ "${EUID}" -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  elif command -v doas >/dev/null 2>&1; then
    SUDO="doas"
  else
    echo "Please run as root (or install sudo/doas) to install system packages." >&2
    exit 1
  fi
fi

if command -v apt-get >/dev/null 2>&1; then
  ${SUDO} apt-get update
  if [[ "${MODE}" == "build" ]]; then
    ${SUDO} apt-get install -y \
      build-essential pkg-config curl ca-certificates \
      libwebkit2gtk-4.1-dev libgtk-3-dev libglib2.0-dev \
      libcairo2-dev libgdk-pixbuf-2.0-dev \
      desktop-file-utils shared-mime-info dpkg-dev
  else
    ${SUDO} apt-get install -y \
      libwebkit2gtk-4.1-0 \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good poppler-utils \
      policykit-1 \
      desktop-file-utils shared-mime-info \
      || ${SUDO} apt-get install -y libwebkit2gtk-4.1-0 gstreamer1.0-plugins-base gstreamer1.0-plugins-good poppler-utils polkitd pkexec desktop-file-utils shared-mime-info
    # Ubuntu 22.04 uses libgtk-3-0; Ubuntu 24.04+ uses libgtk-3-0t64 (pulled automatically by libwebkit2gtk-4.1-0)
  fi
elif command -v dnf >/dev/null 2>&1; then
  if [[ "${MODE}" == "build" ]]; then
    ${SUDO} dnf install -y \
      gcc gcc-c++ pkgconf-pkg-config curl ca-certificates \
      webkit2gtk4.1-devel gtk3-devel glib2-devel cairo-devel \
      gdk-pixbuf2-devel gstreamer1-plugins-base gstreamer1-plugins-good poppler-utils \
      desktop-file-utils shared-mime-info rpm-build
  else
    ${SUDO} dnf install -y \
      webkit2gtk4.1 gtk3 polkit gstreamer1-plugins-base gstreamer1-plugins-good poppler-utils \
      desktop-file-utils shared-mime-info
  fi
elif command -v pacman >/dev/null 2>&1; then
  if [[ "${MODE}" == "build" ]]; then
    ${SUDO} pacman -S --needed --noconfirm \
      base-devel rust pkgconf webkit2gtk-4.1 gtk3 polkit \
      gst-plugins-base gst-plugins-good poppler \
      desktop-file-utils shared-mime-info
  else
    ${SUDO} pacman -S --needed --noconfirm \
      webkit2gtk-4.1 gtk3 polkit gst-plugins-base gst-plugins-good poppler \
      desktop-file-utils shared-mime-info
  fi
elif command -v zypper >/dev/null 2>&1; then
  if [[ "${MODE}" == "build" ]]; then
    ${SUDO} zypper --non-interactive install \
      gcc pkg-config curl ca-certificates \
      webkit2gtk3-soup2-devel gtk3-devel gstreamer-plugins-base gstreamer-plugins-good poppler-tools \
      desktop-file-utils shared-mime-info rpm-build \
      || ${SUDO} zypper --non-interactive install gcc pkg-config webkit2gtk4.1-devel gtk3-devel gstreamer-plugins-base gstreamer-plugins-good poppler-tools desktop-file-utils shared-mime-info rpm-build
  else
    ${SUDO} zypper --non-interactive install \
      libwebkit2gtk-4_1-0 gtk3 polkit gstreamer-plugins-base gstreamer-plugins-good poppler-tools \
      desktop-file-utils shared-mime-info
  fi
elif command -v apk >/dev/null 2>&1; then
  if [[ "${MODE}" == "build" ]]; then
    ${SUDO} apk add --no-cache \
      build-base pkgconf curl ca-certificates \
      webkit2gtk-4.1-dev gtk+3.0-dev glib-dev cairo-dev \
      gst-plugins-base gst-plugins-good poppler-utils \
      desktop-file-utils shared-mime-info
  else
    ${SUDO} apk add --no-cache \
      webkit2gtk-4.1 gtk+3.0 polkit doas gst-plugins-base gst-plugins-good poppler-utils \
      desktop-file-utils shared-mime-info
  fi
else
  echo "Unsupported package manager. Ensure runtime packages are installed: webkit2gtk-4.1, gtk3, polkit (or doas)." >&2
  exit 1
fi
