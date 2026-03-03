# Test Fixtures

This directory contains static fixture files used by `edit32` tests and manual verification.

## What is here

- `sample.*`: language-specific sample files for syntax and editing behavior checks.
- `test.*` and `test_brackets.*`: focused edge-case fixtures.
- `data.json` and `config.toml`: structured data fixtures.

## Running tests

Run all automated tests from the repository root:

```sh
cargo test --workspace --all-targets
```

Run lint checks:

```sh
cargo clippy --workspace --all-targets
```
