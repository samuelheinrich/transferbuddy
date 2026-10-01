# Versioning

For every completed set of requested project changes, increment the patch
version in `Cargo.toml` (for example, 0.2.1 → 0.2.2) and update `Cargo.lock`.
Display the full major.minor.patch version in the TUI and CLI. Never hide the
patch component or leave the version unchanged after a project change.
