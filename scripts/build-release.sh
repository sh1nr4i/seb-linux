#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${SEB_VERSION:-0.6.3}"
DIST_DIR="${ROOT_DIR}/dist"
BUILD_ALL=1
BUILD_DEB=0
BUILD_RPM=0
BUILD_TARBALL=0
BUILD_LOCAL=0

for arg in "$@"; do
  case "${arg}" in
    --all)
      BUILD_ALL=1
      ;;
    --deb)
      BUILD_ALL=0
      BUILD_DEB=1
      ;;
    --rpm)
      BUILD_ALL=0
      BUILD_RPM=1
      ;;
    --tarball)
      BUILD_ALL=0
      BUILD_TARBALL=1
      ;;
    --local)
      BUILD_ALL=0
      BUILD_LOCAL=1
      ;;
    *)
      echo "Usage: $0 [--all | --deb | --rpm | --tarball | --local]" >&2
      exit 1
      ;;
  esac
done

if [[ "${BUILD_ALL}" -eq 1 ]]; then
  BUILD_DEB=1
  BUILD_RPM=1
  BUILD_TARBALL=1
fi

mkdir -p "${DIST_DIR}"

stage_bundle_tree() {
  local bin_dir="$1"
  local stage="$2"
  rm -rf "${stage}"
  mkdir -p \
    "${stage}/bin" \
    "${stage}/share/polkit-1/actions" \
    "${stage}/share/applications" \
    "${stage}/share/mime/packages" \
    "${stage}/share/metainfo" \
    "${stage}/share/icons/hicolor/64x64/apps" \
    "${stage}/share/icons/hicolor/256x256/apps"

  cp -f "${bin_dir}/safe-exam-browser" "${stage}/bin/safe-exam-browser"
  cp -f "${bin_dir}/safe-exam-browser-lockdown" "${stage}/bin/safe-exam-browser-lockdown"
  cp -f "${bin_dir}/libseb_web_extension.so" "${stage}/bin/libseb_web_extension.so"
  chmod 0755 "${stage}/bin/safe-exam-browser" "${stage}/bin/safe-exam-browser-lockdown" "${stage}/bin/libseb_web_extension.so"

  cp -f "${ROOT_DIR}/packaging/linux/org.safeexambrowser.lockdown.policy" "${stage}/share/polkit-1/actions/"
  cp -f "${ROOT_DIR}/packaging/linux/safe-exam-browser.desktop" "${stage}/share/applications/"
  cp -f "${ROOT_DIR}/packaging/linux/safe-exam-browser.xml" "${stage}/share/mime/packages/"
  cp -f "${ROOT_DIR}/packaging/linux/safe-exam-browser.metainfo.xml" "${stage}/share/metainfo/"
  cp -f "${ROOT_DIR}/assets/icons/safe-exam-browser-64.png" "${stage}/share/icons/hicolor/64x64/apps/safe-exam-browser.png"
  cp -f "${ROOT_DIR}/assets/icons/safe-exam-browser.png" "${stage}/share/icons/hicolor/256x256/apps/safe-exam-browser.png"
  cp -f "${ROOT_DIR}/packaging/linux/install.sh" "${stage}/install.sh"
  cp -f "${ROOT_DIR}/packaging/linux/uninstall.sh" "${stage}/uninstall.sh"
  chmod 0755 "${stage}/install.sh" "${stage}/uninstall.sh"
}

if [[ "${BUILD_LOCAL}" -eq 1 ]]; then
  "${ROOT_DIR}/scripts/build.sh" --user
  stage_dir="${ROOT_DIR}/build/stage-local/safe-exam-browser-${VERSION}-linux-x86_64"
  stage_bundle_tree "${ROOT_DIR}/build/bin" "${stage_dir}"
  tar -C "$(dirname "${stage_dir}")" -czf "${DIST_DIR}/safe-exam-browser-${VERSION}-local-x86_64.tar.gz" "$(basename "${stage_dir}")"
  echo "Created local tarball: ${DIST_DIR}/safe-exam-browser-${VERSION}-local-x86_64.tar.gz"
fi

if [[ "${BUILD_DEB}" -eq 1 || "${BUILD_TARBALL}" -eq 1 ]]; then
  echo "Building Ubuntu 22.04 LTS Docker image for GLIBC-portable .deb & universal tarball..."
  docker build -t seb-rust-ubuntu22 -f "${ROOT_DIR}/packaging/docker/Dockerfile.ubuntu22" "${ROOT_DIR}"

  cargo_cache="${HOME}/.cargo/registry"
  mkdir -p "${cargo_cache}"
  docker run --rm \
    -v "${ROOT_DIR}:/src" \
    -v "${cargo_cache}:/root/.cargo/registry" \
    -v "seb-target-ubuntu22:/target-ubuntu22" \
    -e "SEB_VERSION=${VERSION}" \
    -e "BUILD_DEB=${BUILD_DEB}" \
    -e "BUILD_TARBALL=${BUILD_TARBALL}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    seb-rust-ubuntu22 \
    bash -ceu '
      cd /src
      cargo build --release --target-dir /target-ubuntu22 -p seb-app -p seb-lockdown-helper -p seb-web-extension
      OUT=/target-ubuntu22/release

      if [ "${BUILD_TARBALL}" = "1" ]; then
        STAGE="/tmp/seb-stage/safe-exam-browser-${SEB_VERSION}-linux-x86_64"
        rm -rf /tmp/seb-stage
        mkdir -p "${STAGE}/bin" "${STAGE}/share/polkit-1/actions" "${STAGE}/share/applications" \
          "${STAGE}/share/mime/packages" "${STAGE}/share/metainfo" \
          "${STAGE}/share/icons/hicolor/64x64/apps" "${STAGE}/share/icons/hicolor/256x256/apps"
        cp -f "${OUT}/safe-exam-browser" "${OUT}/safe-exam-browser-lockdown" "${OUT}/libseb_web_extension.so" "${STAGE}/bin/"
        chmod 0755 "${STAGE}/bin/"*
        cp -f packaging/linux/org.safeexambrowser.lockdown.policy "${STAGE}/share/polkit-1/actions/"
        cp -f packaging/linux/safe-exam-browser.desktop "${STAGE}/share/applications/"
        cp -f packaging/linux/safe-exam-browser.xml "${STAGE}/share/mime/packages/"
        cp -f packaging/linux/safe-exam-browser.metainfo.xml "${STAGE}/share/metainfo/"
        cp -f assets/icons/safe-exam-browser-64.png "${STAGE}/share/icons/hicolor/64x64/apps/safe-exam-browser.png"
        cp -f assets/icons/safe-exam-browser.png "${STAGE}/share/icons/hicolor/256x256/apps/safe-exam-browser.png"
        cp -f packaging/linux/install.sh packaging/linux/uninstall.sh "${STAGE}/"
        chmod 0755 "${STAGE}/install.sh" "${STAGE}/uninstall.sh"
        tar -C /tmp/seb-stage -czf "/src/dist/safe-exam-browser-${SEB_VERSION}-linux-x86_64.tar.gz" "safe-exam-browser-${SEB_VERSION}-linux-x86_64"
      fi

      if [ "${BUILD_DEB}" = "1" ]; then
        DEB_ROOT="/tmp/seb-deb"
        rm -rf "${DEB_ROOT}"
        mkdir -p "${DEB_ROOT}/DEBIAN" "${DEB_ROOT}/usr/bin" "${DEB_ROOT}/usr/lib/safe-exam-browser" \
          "${DEB_ROOT}/usr/share/polkit-1/actions" "${DEB_ROOT}/usr/share/applications" \
          "${DEB_ROOT}/usr/share/mime/packages" "${DEB_ROOT}/usr/share/metainfo" \
          "${DEB_ROOT}/usr/share/icons/hicolor/64x64/apps" "${DEB_ROOT}/usr/share/icons/hicolor/256x256/apps" \
          "${DEB_ROOT}/usr/share/pixmaps"
        install -Dm755 "${OUT}/safe-exam-browser" "${DEB_ROOT}/usr/bin/safe-exam-browser"
        install -Dm755 "${OUT}/safe-exam-browser-lockdown" "${DEB_ROOT}/usr/bin/safe-exam-browser-lockdown"
        install -Dm755 "${OUT}/libseb_web_extension.so" "${DEB_ROOT}/usr/bin/libseb_web_extension.so"
        install -Dm755 "${OUT}/libseb_web_extension.so" "${DEB_ROOT}/usr/lib/safe-exam-browser/libseb_web_extension.so"
        install -Dm644 packaging/linux/org.safeexambrowser.lockdown.policy "${DEB_ROOT}/usr/share/polkit-1/actions/org.safeexambrowser.lockdown.policy"
        install -Dm644 packaging/linux/safe-exam-browser.desktop "${DEB_ROOT}/usr/share/applications/safe-exam-browser.desktop"
        install -Dm644 packaging/linux/safe-exam-browser.xml "${DEB_ROOT}/usr/share/mime/packages/safe-exam-browser.xml"
        install -Dm644 packaging/linux/safe-exam-browser.metainfo.xml "${DEB_ROOT}/usr/share/metainfo/safe-exam-browser.metainfo.xml"
        install -Dm644 assets/icons/safe-exam-browser-64.png "${DEB_ROOT}/usr/share/icons/hicolor/64x64/apps/safe-exam-browser.png"
        install -Dm644 assets/icons/safe-exam-browser.png "${DEB_ROOT}/usr/share/icons/hicolor/256x256/apps/safe-exam-browser.png"
        install -Dm644 assets/icons/safe-exam-browser.png "${DEB_ROOT}/usr/share/pixmaps/safe-exam-browser.png"

        cat > "${DEB_ROOT}/DEBIAN/control" <<EOF
Package: safe-exam-browser
Version: ${SEB_VERSION}
Section: education
Priority: optional
Architecture: amd64
Maintainer: SEB Linux Contributors <noreply@example.org>
Depends: libc6 (>= 2.35), libwebkit2gtk-4.1-0, libgtk-3-0 | libgtk-3-0t64, polkitd | policykit-1, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, shared-mime-info, desktop-file-utils
Description: Safe Exam Browser for Linux (Rust, GTK3 + WebKitGTK)
 Opens .seb exam configuration files, handles seb:// and sebs:// links,
 and enforces kernel-level anti-cheat lockdown.
EOF
        cat > "${DEB_ROOT}/DEBIAN/postinst" <<\EOF
#!/bin/sh
set -e
update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
update-mime-database /usr/share/mime >/dev/null 2>&1 || true
gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || true
EOF
        chmod 0755 "${DEB_ROOT}/DEBIAN/postinst"
        dpkg-deb --build --root-owner-group "${DEB_ROOT}" "/src/dist/safe-exam-browser_${SEB_VERSION}_amd64.deb"
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /src/dist
    '
fi

if [[ "${BUILD_RPM}" -eq 1 ]]; then
  echo "Building Fedora 40 Docker image for .rpm package..."
  docker build -t seb-rust-fedora -f "${ROOT_DIR}/packaging/docker/Dockerfile.fedora" "${ROOT_DIR}"

  cargo_cache="${HOME}/.cargo/registry"
  mkdir -p "${cargo_cache}"
  docker run --rm \
    -v "${ROOT_DIR}:/src" \
    -v "${cargo_cache}:/root/.cargo/registry" \
    -v "seb-target-fedora:/target-fedora" \
    -e "SEB_VERSION=${VERSION}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    seb-rust-fedora \
    bash -ceu '
      cd /src
      cargo build --release --target-dir /target-fedora -p seb-app -p seb-lockdown-helper -p seb-web-extension
      OUT=/target-fedora/release
      TOP=/tmp/rpmbuild
      rm -rf "${TOP}"
      mkdir -p "${TOP}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

      cat > "${TOP}/SPECS/safe-exam-browser.spec" <<EOF
Name:           safe-exam-browser
Version:        ${SEB_VERSION}
Release:        1%{?dist}
Summary:        Safe Exam Browser for Linux (Rust, GTK3 + WebKitGTK)
License:        MPL-2.0
Requires:       webkit2gtk4.1, gtk3, polkit, gstreamer1-plugins-base, gstreamer1-plugins-good, desktop-file-utils, shared-mime-info

%description
Opens .seb exam configuration files, handles seb:// and sebs:// links,
and enforces kernel-level anti-cheat lockdown on Linux.

%install
install -Dm755 ${OUT}/safe-exam-browser %{buildroot}/usr/bin/safe-exam-browser
install -Dm755 ${OUT}/safe-exam-browser-lockdown %{buildroot}/usr/bin/safe-exam-browser-lockdown
install -Dm755 ${OUT}/libseb_web_extension.so %{buildroot}/usr/bin/libseb_web_extension.so
install -Dm755 ${OUT}/libseb_web_extension.so %{buildroot}/usr/lib/safe-exam-browser/libseb_web_extension.so
install -Dm644 /src/packaging/linux/org.safeexambrowser.lockdown.policy %{buildroot}/usr/share/polkit-1/actions/org.safeexambrowser.lockdown.policy
install -Dm644 /src/packaging/linux/safe-exam-browser.desktop %{buildroot}/usr/share/applications/safe-exam-browser.desktop
install -Dm644 /src/packaging/linux/safe-exam-browser.xml %{buildroot}/usr/share/mime/packages/safe-exam-browser.xml
install -Dm644 /src/packaging/linux/safe-exam-browser.metainfo.xml %{buildroot}/usr/share/metainfo/safe-exam-browser.metainfo.xml
install -Dm644 /src/assets/icons/safe-exam-browser-64.png %{buildroot}/usr/share/icons/hicolor/64x64/apps/safe-exam-browser.png
install -Dm644 /src/assets/icons/safe-exam-browser.png %{buildroot}/usr/share/icons/hicolor/256x256/apps/safe-exam-browser.png
install -Dm644 /src/assets/icons/safe-exam-browser.png %{buildroot}/usr/share/pixmaps/safe-exam-browser.png

%post
update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
update-mime-database /usr/share/mime >/dev/null 2>&1 || true
gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || true

%files
/usr/bin/safe-exam-browser
/usr/bin/safe-exam-browser-lockdown
/usr/bin/libseb_web_extension.so
/usr/lib/safe-exam-browser/libseb_web_extension.so
/usr/share/polkit-1/actions/org.safeexambrowser.lockdown.policy
/usr/share/applications/safe-exam-browser.desktop
/usr/share/mime/packages/safe-exam-browser.xml
/usr/share/metainfo/safe-exam-browser.metainfo.xml
/usr/share/icons/hicolor/64x64/apps/safe-exam-browser.png
/usr/share/icons/hicolor/256x256/apps/safe-exam-browser.png
/usr/share/pixmaps/safe-exam-browser.png
EOF
      rpmbuild --define "_topdir ${TOP}" -bb "${TOP}/SPECS/safe-exam-browser.spec"
      cp -f "${TOP}"/RPMS/*/*.rpm /src/dist/
      chown -R "${HOST_UID}:${HOST_GID}" /src/dist
    '
fi

echo "Release artifacts written to ${DIST_DIR}:"
ls -lh "${DIST_DIR}"
