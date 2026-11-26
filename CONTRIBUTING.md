# Contributing

## Development Setup

Install required system libraries (`gtk3`, `webkit2gtk-4.1`, `polkit`, `pkg-config`, Rust toolchain):

```bash
./scripts/dependencies.sh
```

## Building & Testing

- **User Build (Production, mandatory anti-cheat lockdown):**
  ```bash
  ./scripts/build.sh
  ```
- **Dev Build (Local testing with `--dev-bypass` and `--anti-cheat`):**
  ```bash
  ./scripts/build.sh --dev
  ```
- **Run Workspace Unit Tests:**
  ```bash
  cargo test --workspace
  ```

Run the local binary (never run with `sudo`):

```bash
./build/bin/safe-exam-browser
```

## Pull Requests

- Keep changes in Rust (no C/C++, CMake, or Qt files).
- Never include personal or institutional domains/URLs in code, tests, docs, or commits (use `lms.example.edu` or `example.org`).
- Keep commits focused per feature or fix.
- Update documentation when behavior, CLI options, or packaging changes.
- Include verification notes (`cargo test --workspace`, build checks) in the PR description.

## Security

Do not publish sensitive security issues in public issues. See [SECURITY.md](SECURITY.md).
