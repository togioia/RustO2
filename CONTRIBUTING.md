# Contributing to RustO2

Thanks for your interest in RustO2. Bug reports, documentation fixes, and
focused pull requests are all welcome.

## Before you start

- For security issues, do **not** open a public issue. Follow [SECURITY.md](SECURITY.md).
- For larger changes, open an issue first so we can agree on the approach.
- Be respectful. All participants are expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

RustO2 uses the Rust toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml).
With `rustup` installed, the correct toolchain is selected automatically.

```bash
make test     # cargo test --workspace --all-targets
make fmt      # cargo fmt --all
make clippy   # cargo clippy --workspace --all-targets -- -D warnings
```

To run the full stack locally with Docker Compose:

```bash
make up       # build and start the gateway and identity server
make smoke    # run the smoke checks against the running stack
make down
```

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Keep the change focused. Unrelated refactors belong in a separate PR.
3. Add or update tests for behaviour you change or fix.
4. Update documentation (`README.md`, `README.it.md`, `docs/`, `CHANGELOG.md`)
   when user-facing behaviour changes.
5. Make sure `make fmt`, `make clippy` and `make test` pass. CI runs the same checks.
6. Fill in the pull request template.

### Commit messages

Use short, imperative subjects, optionally with a conventional prefix such as
`feat:`, `fix:`, `docs:`, `ci:` or `chore:`. The existing history follows this
style, for example `docs: record the v0.3.0 release benchmark`.

## Reporting bugs and requesting features

Use the issue templates. For questions and ideas that are not yet concrete,
use [Discussions](https://github.com/togioia/RustO2/discussions).

## License

By contributing, you agree that your contributions are licensed under the
[Apache-2.0](LICENSE) license of this project.
