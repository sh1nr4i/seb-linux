#!/usr/bin/env bash
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1; then
    exec sudo "$0" "$@"
  elif command -v doas >/dev/null 2>&1; then
    exec doas "$0" "$@"
  else
    echo "Please run as root." >&2
    exit 1
  fi
fi

rm -f \
  /usr/bin/safe-exam-browser \
  /usr/bin/safe-exam-browser-lockdown \
  /usr/bin/libseb_web_extension.so \
  /usr/lib/safe-exam-browser/libseb_web_extension.so \
  /usr/share/polkit-1/actions/org.safeexambrowser.lockdown.policy \
  /usr/share/applications/safe-exam-browser.desktop \
  /usr/share/mime/packages/safe-exam-browser.xml \
  /usr/share/metainfo/safe-exam-browser.metainfo.xml \
  /usr/share/icons/hicolor/64x64/apps/safe-exam-browser.png \
  /usr/share/icons/hicolor/256x256/apps/safe-exam-browser.png \
  /usr/share/pixmaps/safe-exam-browser.png
rmdir --ignore-fail-on-non-empty /usr/lib/safe-exam-browser 2>/dev/null || true

update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
update-mime-database /usr/share/mime >/dev/null 2>&1 || true
gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || true

echo "Safe Exam Browser removed."
