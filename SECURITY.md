# Security Policy

## Reporting a vulnerability

Please report security issues privately through GitHub Security Advisories. Do not include real user paths, file names, credentials, or scan databases in a public issue.

## Security boundaries

- Filesystem metadata is untrusted input.
- The application must not execute file names or paths.
- Scanning is read-only and does not inspect file contents.
- Logs and diagnostics must not expose full paths by default.
- Privileged scanning, when implemented, will be isolated from the main UI process.
