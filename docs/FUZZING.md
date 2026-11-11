# Fuzzing RustO2

RustO2 uses [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer)
for the parsers that consume untrusted input: JWT headers and token segments,
and the `${VAR}` configuration expander.

The `fuzz/` directory is a **standalone Cargo workspace**. The root workspace
excludes it, so `cargo build`, `cargo test --workspace` and CI never try to
compile libFuzzer. Fuzzing always runs from the repository root with an
explicit nightly toolchain.

## Prerequisites

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

`cargo-fuzz` needs a C toolchain (`cc`/`clang`) to link the libFuzzer runtime.
On Debian/Ubuntu:

```bash
sudo apt-get install build-essential clang
```

## Running the targets

All commands are run from the repository root (`RustO2/`):

```bash
# Build only; useful as a compile check.
cargo +nightly fuzz build jwt_header

# Run for 60 seconds (add -max_total_time=... to bound the run).
cargo +nightly fuzz run jwt_header -- -max_total_time=60
cargo +nightly fuzz run env_expand -- -max_total_time=60

# List targets known to the fuzz project.
cargo +nightly fuzz list
```

Without `-max_total_time`, libFuzzer runs until interrupted with `Ctrl-C`.

## Targets

| Target | Entry point | What it covers |
|---|---|---|
| `jwt_header` | `fuzz/fuzz_targets/jwt_header.rs` | `jsonwebtoken::decode_header` on the whole token and on each segment, plus base64url decoding and JSON parsing of every segment. |
| `env_expand` | `fuzz/fuzz_targets/env_expand.rs` | The production `rusto2_common::env::expand_env` parser. No environment mutation. |

Both targets assert absence of panics. `jwt_header` deliberately treats every
failure as an `Err`/`None`; a panic is a bug. The scheduled `Fuzz` GitHub
Actions workflow runs both targets weekly and uploads crash artifacts.

## Corpus and artifacts

`fuzz/.gitignore` excludes generated state:

```text
target/
corpus/
artifacts/
coverage/
```

- **Corpus:** `fuzz/corpus/<target>/` is maintained automatically by libFuzzer.
  To seed it, copy real examples in before a run, for example a valid JWT
  header segment and a few invalid `{"alg":...}` variants:

  ```bash
  mkdir -p fuzz/corpus/jwt_header
  printf '%s' 'eyJhbGciOiJFZERTQSIsImtpZCI6ImsxIn0' \
    > fuzz/corpus/jwt_header/eddsa-header
  cargo +nightly fuzz run jwt_header fuzz/corpus/jwt_header -- -max_total_time=60
  ```

  The same approach works for `env_expand` with YAML snippets containing
  `${VAR}`, `${VAR:-default}`, unclosed placeholders and invalid names.

- **Crashes:** libFuzzer writes reproducers to `fuzz/artifacts/<target>/`.
  Reproduce one with:

  ```bash
  cargo +nightly fuzz run jwt_header fuzz/artifacts/jwt_header/<crash-file>
  ```

  Keep the reproducer (or a minimised version) as a regression test, then add
  a seed to the corpus if it is small and stable.

- **Coverage:** `cargo +nightly fuzz coverage jwt_header` writes profiles used
  by `cargo cov`/`llvm-cov`; the output is ignored by git.

## Adding a target

1. Create `fuzz/fuzz_targets/<name>.rs` starting with `#![no_main]`, import
   `libfuzzer_sys::fuzz_target`, and keep the body panic-free on arbitrary
   bytes.
2. Prefer calling production code. Add a `path` dependency in
   `fuzz/Cargo.toml` if the parser lives in a crate in this repository; do not
   copy the parser into the fuzz target.
3. Add a `[[bin]]` entry with `name`, `path`, and `test = false`, `doc = false`,
   `bench = false`.
4. Run `cargo +nightly fuzz build <name>` and then
   `cargo +nightly fuzz run <name> -- -max_total_time=60`.
5. Document the target in the table above.

## Validation versus fuzzing

The normal workspace checks stay fast and do not need nightly:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Because `fuzz` is excluded from the root workspace, those commands do not
build libFuzzer or the fuzz targets.
