#syntax=docker/dockerfile:1.2@sha256:e2a8561e419ab1ba6b2fe6cbdf49fd92b95912df1cf7d313c3e2230a333fdbcc
FROM docker.io/library/rust:1.49@sha256:a50165ea96983c21832578afb1c8c028674c965bc1ed43b607871b1f362e06a5 AS solana

RUN sed -i \
      -e 's|deb.debian.org/debian|archive.debian.org/debian|g' \
      -e 's|security.debian.org/debian-security|archive.debian.org/debian-security|g' \
      -e '/buster-updates/d' \
      /etc/apt/sources.list && \
    apt-get -o Acquire::Check-Valid-Until=false update && \
    apt-get install -y \
    bzip2 \
    clang \
    libssl-dev \
    libudev-dev \
    llvm \
    pkg-config \
    zlib1g-dev \
    && \
    rm -rf /var/lib/apt/lists/* && \
    rustup component add rustfmt && \
    rustup default nightly-2021-12-03

# Support additional root CAs
COPY devnet_setup.sh cert.pem* /certs/
# Debian
RUN if [ -e /certs/cert.pem ]; then cp /certs/cert.pem /etc/ssl/certs/ca-certificates.crt; fi

# release.solana.com is decommissioned; install the same official v1.9.4 release
# directly from the GitHub release asset and lay it out where the installer would
# have (active_release), so PATH and the strip.sh reference below keep working.
RUN mkdir -p /root/.local/share/solana/install /tmp/sol && \
    curl -sSfL https://github.com/solana-labs/solana/releases/download/v1.9.4/solana-release-x86_64-unknown-linux-gnu.tar.bz2 -o /tmp/sol.tar.bz2 && \
    tar -xjf /tmp/sol.tar.bz2 -C /tmp/sol && \
    mv /tmp/sol/solana-release /root/.local/share/solana/install/active_release && \
    rm -rf /tmp/sol /tmp/sol.tar.bz2

ENV PATH="/root/.local/share/solana/install/active_release/bin:$PATH"

# Solana does a questionable download at the beginning of a *first* build-bpf call. Trigger and layer-cache it explicitly.
RUN cargo init --lib /tmp/decoy-crate && \
    cd /tmp/decoy-crate && cargo build-bpf && \
    rm -rf /tmp/decoy-crate

# The strip shell script downloads criterion the first time it runs so cache it here as well.
RUN touch /tmp/foo.so && \
    /root/.local/share/solana/install/active_release/bin/sdk/bpf/scripts/strip.sh /tmp/foo.so /tmp/bar.so || \
    rm /tmp/foo.so

# Add bridge contract sources
WORKDIR /usr/src/bridge

ADD . .

ENV RUST_LOG="solana_runtime::system_instruction_processor=trace,solana_runtime::message_processor=trace,solana_bpf_loader=debug,solana_rbpf=debug"
ENV RUST_BACKTRACE=1

FROM solana AS builder

RUN mkdir -p /opt/solana/deps

ENV EMITTER_ADDRESS="11111111111111111111111111111115"
ARG BRIDGE_ADDRESS
RUN [ -n "${BRIDGE_ADDRESS}" ]

# Build Wormhole Solana programs
RUN --mount=type=cache,target=target,id=build \
    cargo build-bpf --manifest-path "bridge/program/Cargo.toml" -- --locked && \
    cargo build-bpf --manifest-path "bridge/cpi_poster/Cargo.toml" -- --locked && \
    cargo build-bpf --manifest-path "modules/token_bridge/program/Cargo.toml" -- --locked && \
    cargo build-bpf --manifest-path "modules/nft_bridge/program/Cargo.toml" -- --locked && \
    cargo build-bpf --manifest-path "migration/Cargo.toml" -- --locked && \
    cp target/deploy/bridge.so /opt/solana/deps/bridge.so && \
    cp target/deploy/cpi_poster.so /opt/solana/deps/cpi_poster.so && \
    cp target/deploy/wormhole_migration.so /opt/solana/deps/wormhole_migration.so && \
    cp target/deploy/token_bridge.so /opt/solana/deps/token_bridge.so && \
    cp target/deploy/nft_bridge.so /opt/solana/deps/nft_bridge.so && \
    cp modules/token_bridge/token-metadata/spl_token_metadata.so /opt/solana/deps/spl_token_metadata.so

# This stage is skipped in normal builds and needs to be explicitly invoked
# (like `DOCKER_BUILDKIT=1 docker build --target ci_tests .`).
FROM solana AS ci_tests

# This emitter address is necessary for the governance tests.
ENV EMITTER_ADDRESS="CiByUvEcx7w2HA4VHcPCBUAFQ73Won9kB36zW9VjirSr"

ARG BRIDGE_ADDRESS
RUN [ -n "${BRIDGE_ADDRESS}" ]

# This is a pre-built contract.
RUN --mount=type=cache,target=target,id=test \
    mkdir -p target/deploy && \
    cp modules/token_bridge/token-metadata/spl_token_metadata.so target/deploy/

RUN --mount=type=cache,target=target,id=test \
    cargo test-bpf \
        --manifest-path bridge/program/Cargo.toml \
        --features trace,instructions

RUN --mount=type=cache,target=target,id=test \
    cargo test-bpf \
        --manifest-path modules/token_bridge/program/Cargo.toml \
        --features trace,instructions

RUN --mount=type=cache,target=target,id=test \
    cargo test-bpf \
        --manifest-path modules/nft_bridge/program/Cargo.toml \
        --features trace,instructions

FROM scratch AS export-stage
COPY --from=builder /opt/solana/deps /

