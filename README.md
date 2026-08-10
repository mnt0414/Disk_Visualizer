# Disk Visualizer

A lightweight, fast, offline disk usage visualizer for macOS and Windows.

> Status: Phase 0 bootstrap. The application shell and build infrastructure are being established.

## Targets

- macOS latest two releases on Apple Silicon (`arm64`)
- Windows 11 (`x86_64`)

## Stack

- Rust
- Tauri 2
- React
- TypeScript
- Vite
- SQLite (introduced in a later phase)

## Development

```bash
npm install
npm run tauri dev
```

PNG and ICO assets required by Tauri are generated locally from the versioned icon source before development and production builds.

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
