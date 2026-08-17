# Disk Visualizer

A lightweight, fast, offline disk usage visualizer for macOS and Windows.

> Status: Phase 5 application-cache recognition is complete. Read-only scanning, saved history, versioned cache classification, and runtime-state display are available for manual testing. Phase 6 incremental scanning is next.

## Targets

- macOS latest two releases on Apple Silicon (`arm64`)
- Windows 11 (`x86_64`)

## Stack

- Rust
- Tauri 2
- React
- TypeScript
- Vite
- SQLite

## Development

Use Node.js 22 and the stable Rust toolchain.

```bash
npm install
npm run tauri dev
```

PNG and ICO assets required by Tauri are generated locally from the versioned icon source before development and production builds.

For a safe first run and the current expected behavior, see [`docs/MANUAL_TESTING.md`](docs/MANUAL_TESTING.md).

Quality checks:

```bash
npm run format:check
npm run check
npm test
npm run icons
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

## Privacy and safety

Disk Visualizer is designed to work offline. It does not delete files, inspect file contents, or send file names and paths to external services.

## License

Apache License 2.0. See `LICENSE` and `NOTICE`.
