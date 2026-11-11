# Supply chain and release provenance

RustO2 publishes signed container images to GitHub Container Registry (GHCR)
and attaches an SBOM plus SLSA build provenance to every GitHub release. This
document explains what is produced, how to verify it, and which checks run in
CI.

## What CI enforces

| Workflow | Trigger | Blocking | Purpose |
|---|---|---|---|
| `ci.yml` / `rust` | push to `main`, pull request | yes | `cargo fmt`, `cargo clippy -D warnings`, `cargo test` |
| `ci.yml` / `docker` | push to `main`, pull request | yes | build both distroless images |
| `ci.yml` / `deny` | push to `main`, pull request | yes | `cargo deny check` from `deny.toml` |
| `ci.yml` / `audit` | push to `main`, pull request | **no** | RustSec advisory scan (`cargo audit`) |
| `release.yml` | `v*` tag, or `workflow_dispatch` from a tag | n/a | build, push, sign, attest, SBOM, release |
| `benchmark.yml` | `v*` tag, or `workflow_dispatch` | **no** | `oha` benchmark report artifact |

The advisory scan is deliberately non-blocking: a newly published advisory
should be visible immediately, but it should not make an unrelated pull request
unmergeable before a dependency upgrade is available. `cargo-deny` is the
blocking policy gate because `deny.toml` is reviewed and versioned.

All workflows use `permissions: contents: read` by default. Only the release
job elevates permissions, and only for the operations it performs:

```yaml
permissions:
  contents: write      # create/upload the GitHub release
  packages: write      # push images and attestations to GHCR
  id-token: write      # keyless cosign + GitHub attestations
  attestations: write  # actions/attest-build-provenance
```

## Container images

The release workflow publishes two images, with the repository owner
lower-cased for GHCR:

```text
ghcr.io/<owner>/rusto2-gw
ghcr.io/<owner>/rusto2-is
```

For this repository that is:

```text
ghcr.io/togioia/rusto2-gw
ghcr.io/togioia/rusto2-is
```

Tags produced by `docker/metadata-action` for a release tag such as `v0.3.0`:

| Tag | Example | Notes |
|---|---|---|
| semantic version | `0.1.0` | always |
| major/minor | `0.1` | stable releases only |
| commit | `sha-<full 40-char sha>` | always |
| `latest` | `latest` | stable releases only, not pre-releases |

Images are built for `linux/amd64` only. The Dockerfiles compile
`x86_64-unknown-linux-musl` explicitly, so adding `linux/arm64` requires a
Dockerfile/cross-compilation change, not just a workflow change. Pull requests
never push: `release.yml` has no `pull_request` trigger and every push step is
in the release job.

GHCR packages created by a workflow are private by default. If anonymous
`docker pull` should work, set the package visibility to public in the GitHub
package settings. Private packages work too; authenticate with a token that has
`read:packages` before pulling or verifying.

`workflow_dispatch` is intended for re-running a release from an existing tag.
Dispatch the workflow **on the tag ref**, for example:

```bash
gh workflow run release.yml --repo togioia/RustO2 --ref v0.3.0
```

The prepare job rejects a branch ref. This is deliberate: the cosign
certificate identity and the GitHub attestation identity are tied to the ref,
so a release must always come from `refs/tags/<tag>`.

## SBOMs

For each pushed image digest, `anchore/sbom-action` runs Syft
(`syft-version: v1.57.1`) and writes an SPDX 2.3 JSON document:

```text
rusto2-gw.spdx.json
rusto2-is.spdx.json
```

The files are:

1. uploaded as the workflow artifact `rusto2-release-evidence`;
2. attached to the GitHub release for the tag.

Download and inspect them with the GitHub CLI:

```bash
gh release download v0.3.0 \
  --repo togioia/RustO2 \
  --pattern 'rusto2-*.spdx.json'

jq -r '.packages[].name' rusto2-gw.spdx.json | sort -u
jq -r '.packages[] | select(.versionInfo != null) | "\(.name) \(.versionInfo)"' \
  rusto2-gw.spdx.json | head
```

The SBOM describes the packages Syft found in the final distroless image, not
only the Rust dependency graph. This is intentional: it includes OS packages
and the static binary's embedded components where Syft can identify them.

## Keyless cosign signatures

Images are signed by digest with Sigstore cosign in keyless mode. No long-lived
private key exists in the repository. GitHub Actions requests a short-lived OIDC
token (`id-token: write`), Fulcio issues a certificate bound to the workflow
identity, and the signature is stored in GHCR alongside the image digest.

The signing identity is always:

```text
https://github.com/<owner>/<repo>/.github/workflows/release.yml@refs/tags/<tag>
```

### Resolve the image digest

Tags are mutable; verify the digest that was actually signed. For example:

```bash
docker buildx imagetools inspect ghcr.io/togioia/rusto2-gw:0.3.0
```

Use the `Digest:` line from the output, for example
`sha256:0123456789abcdef...`.

### Verify the signature

Replace the digest and tag with the values from the release:

```bash
cosign verify \
  --certificate-identity "https://github.com/togioia/RustO2/.github/workflows/release.yml@refs/tags/v0.3.0" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  ghcr.io/togioia/rusto2-gw@sha256:<digest>
```

To verify any release tag without editing the command per tag:

```bash
cosign verify \
  --certificate-identity-regexp '^https://github\.com/togioia/RustO2/\.github/workflows/release\.yml@refs/tags/v[0-9].*$' \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  ghcr.io/togioia/rusto2-gw@sha256:<digest>
```

Do the same for `ghcr.io/togioia/rusto2-is`. A successful verification prints
the signatures and the certificate identity. `cosign verify` also checks the
Rekor transparency log by default.

If the GHCR package is private, authenticate first:

```bash
echo "$GITHUB_TOKEN" | docker login ghcr.io -u "$GITHUB_ACTOR" --password-stdin
```

## SLSA build provenance

`actions/attest-build-provenance@v2` generates SLSA v1 build provenance for each
image digest and pushes it to GHCR (`push-to-registry: true`). It also produces
an attestation bundle, which is attached to the release as:

```text
rusto2-gw.provenance.jsonl
rusto2-is.provenance.jsonl
```

Verify the provenance with the GitHub CLI:

```bash
gh attestation verify \
  oci://ghcr.io/togioia/rusto2-gw@sha256:<digest> \
  --repo togioia/RustO2
```

For the identity server:

```bash
gh attestation verify \
  oci://ghcr.io/togioia/rusto2-is@sha256:<digest> \
  --repo togioia/RustO2
```

`gh attestation verify` checks that the attestation was issued by this
repository's `release.yml` workflow and that the subject digest matches the
image you asked about. Use `--format json` for machine-readable output.

The provenance bundle and the cosign signature are complementary:

- cosign proves **who signed this digest** and stores the signature in the
  registry;
- GitHub attestations record **how the image was built** (workflow, commit,
  builder) and are also available through the GitHub attestation API.

## cargo-audit and cargo-deny

### `deny.toml`

The policy is intentionally explicit:

- **advisories** — vulnerabilities are denied. `unmaintained = "workspace"`
  fails only for unmaintained direct dependencies; unmaintained transitive
  crates stay non-blocking and are still visible in the `cargo audit` job.
  `unsound = "workspace"` and `yanked = "warn"`. There are no advisory ignores
  today.
- **licenses** — the allowlist is the minimal set present in the resolved
  graph: `Apache-2.0`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`,
  `Unicode-3.0`, `Zlib`, `Unlicense`, `CDLA-Permissive-2.0`. `MPL-2.0`,
  `CC0-1.0` and `OpenSSL` are not allowed because nothing in the graph needs
  them. Adding a license is a reviewed policy change, not a blanket allow.
- **bans** — duplicate versions are warnings; wildcard version requirements
  are denied. `allow-wildcard-paths` is enabled for the workspace's internal
  path-only dependencies (see the wrapper note below).
- **sources** — only the crates.io registry is allowed. Git dependencies and
  alternative registries are denied.

Run it locally with the same wrapper used by CI:

```bash
cargo install cargo-deny --locked --version 0.20.2
bash scripts/cargo-deny.sh
```

`scripts/cargo-deny.sh` copies the workspace to a temporary directory and adds
`publish = false` to the internal crate manifests in that copy before running
`cargo deny check`. The checked-in manifests are not modified. The reason is
that `cargo-deny` classifies a crate without `publish = false` as publishable,
and therefore treats its path-only dependency on `rusto2-common` as a wildcard
error. Marking the temporary copy private lets `wildcards = "deny"` remain
strict for every registry dependency. If the checked-in manifests later add
`publish = false`, the wrapper can be replaced by a direct `cargo deny check`.

### cargo-audit

The `Advisory scan (non-blocking)` job runs `cargo audit` after resolving the
full graph. It is intentionally allowed to fail. The only known advisory in
the current graph is:

| Advisory | Crate | Kind | Status |
|---|---|---|---|
| `RUSTSEC-2024-0449` | `unsafe-libyaml` | unmaintained | transitive via `serde_yaml`; replacing `serde_yaml` is a separate source change |

`serde_yaml` itself is deprecated upstream but has no advisory against the
locked version. It remains a direct dependency of the gateway/identity YAML
configuration; migrating it is outside this supply-chain change.

Run the advisory scan locally with:

```bash
cargo install cargo-audit --locked
cargo metadata --format-version 1 > /dev/null
cargo audit
```

## Reproducing the release workflow locally

The release workflow is deliberately not runnable as a single local command
because keyless signing requires a GitHub OIDC identity. The build half can be
reproduced with:

```bash
docker build -f docker/gateway.Dockerfile -t rusto2-gw:local .
docker build -f docker/identity.Dockerfile -t rusto2-is:local .
```

The CI `docker` job runs the same two builds on every pull request, so a
release build failure is normally caught before a tag is pushed.

## Action pins

The workflows use these pinned action majors:

| Action | Version |
|---|---|
| `actions/checkout` | `v7` |
| `actions/upload-artifact` | `v7` |
| `actions/attest-build-provenance` | `v2` |
| `docker/setup-buildx-action` | `v4` |
| `docker/login-action` | `v4` |
| `docker/metadata-action` | `v6` |
| `docker/build-push-action` | `v6` |
| `sigstore/cosign-installer` | `v3` (cosign `v3.1.4`) |
| `anchore/sbom-action` | `v0.24.3` (Syft `v1.57.1`) |
| `taiki-e/install-action` | `v2` |
| `dtolnay/rust-toolchain` | `stable` with toolchain `1.99.0` |
| `Swatinem/rust-cache` | `v2` |

Bump these deliberately. A major-version update can change default behavior
even when the workflow inputs stay the same.
