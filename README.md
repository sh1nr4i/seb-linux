<div align="center">
  <img src="assets/icons/safe-exam-browser.png" alt="Safe Exam Browser for Linux logo" width="120" />
  <h1>Safe Exam Browser for Linux (Pure Rust)</h1>
  <p>A 100% Pure Rust, native Linux client for Safe Exam Browser built with GTK3, WebKitGTK 4.1, and kernel-level anti-cheat lockdown.</p>
</div>

> [!NOTE]
> **Beta Release (`v0.6.1-beta`)**: Core exam flows, Moodle `.seb` reconfiguration, SEB Server, Screen Proctoring, and kernel-level anti-cheat lockdown are implemented and tested, while broader real-world exam scenarios across Linux distributions are still being validated. Please report any issues you encounter.

## Overview

`seb-linux-rust` is a clean, 100% Pure Rust implementation of Safe Exam Browser for Linux with zero C/C++, zero CMake, and zero Qt dependencies. It supports both **Wayland** and **X11** desktops across Debian/Ubuntu, Fedora/RHEL, Arch Linux, openSUSE, and other distributions.

## Key Features

- **100% Pure Rust Workspace**: Modular 9-crate architecture (`seb-app`, `seb-config`, `seb-filter`, `seb-security`, `seb-lockdown-helper`, `seb-platform`, `seb-server`, `seb-proctoring`, `seb-web-extension`).
- **Full `.seb` Configuration & Moodle SSO Support**:
  - Opens local `.seb` files and `seb://` / `sebs://` links, including encrypted RNCryptor v2, zlib/gzip, and PKCS#12/RSA-OAEP client certificates.
  - Preserves browser session cookies during `LoadWithBrowser` SSO login transitions and injects `X-SafeExamBrowser-ConfigKeyHash` and `X-SafeExamBrowser-RequestHash` via a Pure Rust WebKitGTK Web Extension (`libseb_web_extension.so`), plus `window.SafeExamBrowser.security` JS API.
- **Kernel-Level Anti-Cheat Lockdown (`safe-exam-browser-lockdown`)**:
  - Activated automatically in **User Build** via Polkit (`pkexec`) or `doas` — `safe-exam-browser` itself never runs as `root`.
  - Two-phase startup: prohibited-process pre-flight dialog runs before `evdev` keyboard grab so examinees can close blocked apps cleanly.
  - Blocks OS escape shortcuts (`Alt+Tab`, `Alt+F1..F24`, `Ctrl+Alt+*`, `Super`, `PrintScreen`, etc.) via `evdev`/`uinput`, translates `Super+A` to SEB Action Center, and drives SEB's internal `Alt+Tab` window switcher.
  - Blocks 3+/4-finger OS touchpad gestures while cloning physical touchpad identity (`EVIOCGID`/`EVIOCGNAME`) so OS settings (`NaturalScroll`, acceleration, tap-to-click) stay intact.
  - Triple fail-safe automatic unlock: `PR_SET_PDEATHSIG(SIGTERM)`, `stdin` EOF detection, and a 6-second heartbeat watchdog.
- **Native Desktop Integration (Zero External Shell Wrappers)**:
  - Pure Rust D-Bus (`zbus`) for NetworkManager / `iwd` Wi-Fi management (including in-app Wi-Fi connection), `login1`/`elogind`/`ScreenSaver` power inhibition, and Secret Service.
  - Native PulseAudio/PipeWire Unix socket for audio volume/mute, kernel `sysfs` battery monitoring, and `x11rb` XKB/window controls.
- **SEB Server & Window-Only Screen Proctoring**:
  - Non-blocking SEB Server ping/instruction loop (`SEB_QUIT`, `SEB_FORCE_LOCK_SCREEN`, `SEB_PROCTORING`, raise/lower hand, log reporting).
  - Window-only Screen Proctoring via Cairo surface capture (never full OS screen), inactive unless explicitly enabled by config or SEB Server.

## Installation

Pre-built packages are published on the [GitHub Releases](https://github.com/sh1nr4i/seb-linux-rust/releases) page:

- **Debian / Ubuntu (`.deb`)**:
  ```bash
  sudo apt install ./safe-exam-browser_0.6.1_amd64.deb
  ```
- **Fedora / RHEL (`.rpm`)**:
  ```bash
  sudo dnf install ./safe-exam-browser-0.6.1-1.*.x86_64.rpm
  ```
- **Arch Linux (`PKGBUILD`)**:
  ```bash
  cd packaging/arch && makepkg -si
  ```
- **Universal Linux Tarball (`.tar.gz`)**:
  ```bash
  tar -xzf safe-exam-browser-0.6.1-linux-x86_64.tar.gz
  cd safe-exam-browser-0.6.1-linux-x86_64
  sudo ./install.sh
  ```

## Building from Source

### 1. Install System Dependencies

```bash
./scripts/dependencies.sh
```

Required libraries: Rust/Cargo toolchain, `gtk3`, `webkit2gtk-4.1`, `polkit`, `pkg-config`, and standard desktop MIME/icon utilities.

### 2. Build

- **User Build (Production — mandatory anti-cheat lockdown, no dev bypass flags)**:
  ```bash
  ./scripts/build.sh
  ```
- **Dev Build (Local testing — enables `--dev-bypass` and `--anti-cheat` flags)**:
  ```bash
  ./scripts/build.sh --dev
  ```

Build outputs are placed in `build/bin/`:
- `build/bin/safe-exam-browser`
- `build/bin/safe-exam-browser-lockdown`
- `build/bin/libseb_web_extension.so`

### 3. Build Multi-Distro Release Packages (Docker)

```bash
./scripts/build-release.sh --all
```

Outputs `.deb`, `.rpm`, and universal `.tar.gz` archives into `dist/`.

## Usage

Launch the default start page (never run with `sudo`):

```bash
safe-exam-browser
```

Open a local `.seb` configuration file:

```bash
safe-exam-browser /path/to/exam.seb
```

Open an exam via `seb://` or `sebs://` link:

```bash
safe-exam-browser "sebs://lms.example.edu/mod/quiz/accessrule/seb/config.php?cmid=12345"
```

## Workspace Architecture

| Crate | Responsibility |
| --- | --- |
| `crates/seb-app` | Fullscreen frameless GTK3 shell, `BrowserPane` (WebKitGTK 4.1), taskbar, Action Center, internal `Alt+Tab` switcher, dialogs |
| `crates/seb-config` | `.seb` plist/JSON/RNCryptor/PKCS#12/RSA decryption, canonical ConfigKey & BrowserExamKey calculation |
| `crates/seb-filter` | Main-frame & subresource URL filter rules and `.seb` resource detection |
| `crates/seb-security` | Runtime integrity, VM/debugger/display checks, power inhibition (`zbus`), `LockdownClient` |
| `crates/seb-lockdown-helper` | Privileged `evdev`/`uinput` keyboard & touchpad gesture filter and root process scanner |
| `crates/seb-platform` | Pure Rust audio (PulseAudio socket), battery (`sysfs`), Wi-Fi/network (`zbus`), keyboard layout |
| `crates/seb-proctoring` | Window-only Screen Proctoring capture pipeline and metadata encoding |
| `crates/seb-server` | SEB Server OAuth2/API client, ping/instructions, and sanitization |
| `crates/seb-web-extension` | WebKitGTK 4.1 `cdylib` injecting SEB key hashes and enforcing subresource URL filters |

## Testing

```bash
cargo test --workspace
```

## Contributing & Security

- Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).
- To report security vulnerabilities privately, see [SECURITY.md](SECURITY.md) or open a [GitHub Security Advisory](https://github.com/sh1nr4i/seb-linux-rust/security/advisories/new).

## License

This project is licensed under the **Mozilla Public License 2.0 (`MPL-2.0`)**. See [LICENSE](LICENSE) for details.
