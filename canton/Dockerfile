# syntax=docker/dockerfile:1
#
# Canton devnet image for the Wormhole core bridge.
#
# Uses the Canton Network `dpm` toolchain (https://docs.canton.network/sdks-tools/cli-tools/dpm),
# not the legacy `daml` assistant. Stage 1 installs dpm + the SDK pinned in
# daml.yaml and builds the `wormhole-core` DAR; stage 2 runs a Canton sandbox
# (Ledger API v2 on :6865) with the DAR preloaded, and a companion bootstrap step
# (see devnet/canton-devnet.yaml) allocates the guardianGovernance party and creates the
# CoreState via Test.TestCore:setup.
#
# dpm manages the SDK from daml.yaml's `sdk-version` (Daml SDK 3.7.3 → Canton
# 3.7.3, Protocol Versions 36 and 37, Daml-LF 2.3; Ledger API v2 + DA.Crypto.Text
# builtins). Canton is JVM-based, so the base image ships a JDK
# (17+).
#
# NOTE: building this image needs network access to get.digitalasset.com (dpm
# installer) so it is not built in this repo's CI. The dpm commands here
# (`dpm install <version>`, `dpm build`) and the sandbox/script flags used by the
# devnet scripts have been validated against dpm for the pinned SDK.
ARG DAML_SDK_VERSION=3.7.3

FROM eclipse-temurin:17-jdk@sha256:91b6210cce02091f6f0798a83ec51aa223828242c5a21a85793bb8c28dc891c4 AS build
ARG DAML_SDK_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Install the dpm CLI, then the SDK version pinned in daml.yaml.
RUN curl -fsSL https://get.digitalasset.com/install/install.sh | sh
# The installer activates the latest SDK; pin dpm runs outside a daml.yaml project (sandbox, script).
ENV PATH="/root/.dpm/bin:${PATH}" DPM_SDK_VERSION="${DAML_SDK_VERSION}"
WORKDIR /canton-build
COPY multi-package.yaml ./
# Vendored CIP-0056 token-standard interface DARs (core fees data-depend on
# them); must be present before the build.
COPY dars ./dars
COPY core ./core
COPY test ./test
COPY test-lookalike ./test-lookalike
# LocalNet smoke package (localnet/README.md); never uploaded outside LocalNet.
COPY localnet/daml ./localnet/daml
# Multi-package build: core (templates) then test (scripts, data-dep on core
# + the interface DARs). Produces core/.daml/dist/wormhole-core-<version>.dar and
# test/.daml/dist/wormhole-core-test-0.1.0.dar (which packs core's DALFs).
RUN dpm install "${DAML_SDK_VERSION}" && dpm build --all
# LocalNet vetting probes: one package name in two versions, which multi-package.yaml cannot hold.
# Build each outside /canton-build, so dpm does not resolve them against multi-package.yaml.
COPY localnet/vetting_probe /vetting-probe-build
RUN cd /vetting-probe-build/v1 && dpm build && cd /vetting-probe-build/v2 && dpm build

# Go toolchain for the watcher sandbox tests; same image digest as node/Dockerfile.
FROM docker.io/golang:1.25.10-bookworm@sha256:154bd7001b6eb339e88c964442c0ad6ed5e53f09844cc818a41ce4ecb3ce3b43 AS go-toolchain

# Runs `go test -tags integration ./pkg/watchers/canton/...` with dpm, the SDK,
# and a JDK. CI mounts the repository at /src; go.sum pins every module.
FROM build AS watcher-integration
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=go-toolchain /usr/local/go /usr/local/go
ENV PATH="/usr/local/go/bin:${PATH}" GOFLAGS=-mod=readonly GOTOOLCHAIN=local

FROM eclipse-temurin:17-jdk@sha256:91b6210cce02091f6f0798a83ec51aa223828242c5a21a85793bb8c28dc891c4 AS canton
ARG DAML_SDK_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://get.digitalasset.com/install/install.sh | sh
ENV PATH="/root/.dpm/bin:${PATH}" DPM_SDK_VERSION="${DAML_SDK_VERSION}"
WORKDIR /canton
# The production (clean) core DAR, for reference, plus the test DAR — which packs
# core's DALFs and the setup/integration scripts. The bootstrap uploads the test
# DAR (bundling core) and runs Test.TestCore:setup against the devnet sandbox.
# Fresh build stage: the glob matches exactly the version in core/daml.yaml.
COPY --from=build /canton-build/core/.daml/dist/wormhole-core-*.dar /canton/wormhole-core.dar
COPY --from=build /canton-build/test/.daml/dist/wormhole-core-test-0.1.0.dar /canton/wormhole-core-test.dar
COPY devnet/ /canton/devnet/
# Pre-install the SDK so `dpm sandbox` / `dpm script` start without a fetch.
# `dpm install` requires an explicit version argument.
RUN dpm install "${DAML_SDK_VERSION}" && chmod +x /canton/devnet/*.sh
# Ledger API v2 (gRPC).
EXPOSE 6865
CMD ["/canton/devnet/start_sandbox.sh"]
