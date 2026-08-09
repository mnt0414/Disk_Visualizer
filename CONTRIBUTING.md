# Contributing

## Development principles

- Keep all filesystem scanning read-only.
- Do not add telemetry or external transmission of file names, paths, or scan results.
- Do not follow filesystem links or cross volume boundaries without an explicit design decision.
- Include tests for success, error, and cancellation paths.
- Keep macOS and Windows behavior documented.

## Pull requests

1. Create a focused branch from the current development base.
2. Run frontend and Rust quality checks.
3. Describe security, privacy, accessibility, and performance impact.
4. Include light and dark screenshots for UI changes.
5. Keep PRs small enough to review independently.
