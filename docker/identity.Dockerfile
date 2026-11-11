# syntax=docker/dockerfile:1.7
# ---------- build ----------
# Build on glibc so proc-macros stay dynamic, link the final binary against
# musl statically, and ship it in a distroless image.
FROM rust:1.99-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

WORKDIR /src
ENV CARGO_TERM_COLOR=never \
    RUSTFLAGS="-C target-feature=+crt-static"

COPY Cargo.toml Cargo.lock ./
COPY crates/rusto2-common/Cargo.toml crates/rusto2-common/Cargo.toml
COPY crates/rusto2-gw/Cargo.toml crates/rusto2-gw/Cargo.toml
COPY crates/rusto2-is/Cargo.toml crates/rusto2-is/Cargo.toml
COPY crates/rusto2-synapse/Cargo.toml crates/rusto2-synapse/Cargo.toml

# Cache dependency compilation with a throw-away workspace.
RUN mkdir -p crates/rusto2-common/src crates/rusto2-gw/src crates/rusto2-is/src crates/rusto2-synapse/src \
    && echo 'pub fn placeholder() {}' > crates/rusto2-common/src/lib.rs \
    && echo 'fn main() {}' > crates/rusto2-gw/src/main.rs \
    && echo 'fn main() {}' > crates/rusto2-is/src/main.rs \
    && echo 'pub fn placeholder() {}' > crates/rusto2-synapse/src/lib.rs \
    && cargo build --release --target x86_64-unknown-linux-musl -p rusto2-is

COPY crates ./crates
RUN find crates -name '*.rs' -exec touch {} + \
    && cargo build --release --target x86_64-unknown-linux-musl -p rusto2-is \
    && mkdir -p /out/data

# ---------- runtime ----------
FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/rusto2-is /usr/local/bin/rusto2-is
COPY --from=builder --chown=65532:65532 /out/data /data

USER nonroot:nonroot
EXPOSE 9000
ENTRYPOINT ["/usr/local/bin/rusto2-is"]
