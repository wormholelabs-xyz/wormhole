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
# The SDK is the dpm SDK tarball of DAML_SDK_VERSION (Daml SDK 3.7.3, Canton
# 3.7.3, Protocol Versions 36 and 37, Daml-LF 2.3), checked by SHA-256. `dpm
# bootstrap` installs it from the tarball and makes it the active SDK, so dpm
# commands outside a daml.yaml project (sandbox, script) run it too. Canton is
# JVM-based, so the base image ships a JDK. Java 21: the Canton 3.7.3 release
# notes list Java 21 as the tested runtime. eclipse-temurin:21-jdk is
# 21.0.13_9-jdk-resolute (ubuntu:26.04) from adoptium/containers
# a240a79c19afb3da535529e29ab3156d5c08a6e0. The digest is the multi-arch index.
#
# Every daml.yaml under canton/ must pin DAML_SDK_VERSION
# (node/pkg/watchers/canton/daml_sdk_version_test.go).
ARG DAML_SDK_VERSION=3.7.3

FROM eclipse-temurin:21-jdk@sha256:db5cc6341a1d7bc40cdb51e1f0d9824f3bb2cc8107d29da421dc4df7d11f05f1 AS dpm-sdk
ARG DAML_SDK_VERSION
# SHA-256 of dpm-<DAML_SDK_VERSION>-linux-<arch>.tar.gz from the Digital Asset public-generic repository.
ARG DPM_SDK_TARBALL_SHA256_AMD64=73bd3d5e1ad656a2cd89b7c4e122495fd68acd5b84db4a91c5aefb9a2ddeb2e2
ARG DPM_SDK_TARBALL_SHA256_ARM64=2b2ad6c9acdd30fc79dc57622109c49562f485cfc269ad969e1081229fa003ab
# SHA-256 of canton-open-source-3.7.3.jar (node/pkg/cantonclient/README.md).
ARG CANTON_OPEN_SOURCE_JAR_SHA256=3824a19207b7be4aed3120006e0d48bbd5452196ef1d8b77b2d5ec2f086a9528
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    architecture="$(dpkg --print-architecture)"; \
    case "$architecture" in \
      amd64) tarball_sha256="$DPM_SDK_TARBALL_SHA256_AMD64" ;; \
      arm64) tarball_sha256="$DPM_SDK_TARBALL_SHA256_ARM64" ;; \
      *) echo "unsupported architecture $architecture" >&2; exit 1 ;; \
    esac; \
    tarball="dpm-${DAML_SDK_VERSION}-linux-${architecture}.tar.gz"; \
    curl -fsSL --max-time 1800 -o "/tmp/$tarball" \
      "https://artifactregistry.googleapis.com/download/v1/projects/da-images/locations/europe/repositories/public-generic/files/dpm-sdk:${DAML_SDK_VERSION}:${tarball}:download?alt=media"; \
    echo "$tarball_sha256  /tmp/$tarball" | sha256sum -c -; \
    mkdir /tmp/dpm-sdk; \
    tar xzf "/tmp/$tarball" -C /tmp/dpm-sdk --strip-components 1; \
    /tmp/dpm-sdk/bin/dpm bootstrap /tmp/dpm-sdk; \
    rm -rf "/tmp/$tarball" /tmp/dpm-sdk; \
    echo "$CANTON_OPEN_SOURCE_JAR_SHA256  /root/.dpm/cache/components/canton-open-source/${DAML_SDK_VERSION}/lib/canton-open-source-${DAML_SDK_VERSION}.jar" | sha256sum -c -; \
    test "$(/root/.dpm/bin/dpm version --active)" = "$DAML_SDK_VERSION"
ENV PATH="/root/.dpm/bin:${PATH}"

FROM dpm-sdk AS build
ARG DAML_SDK_VERSION
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
RUN test "$(sed -n 's/^sdk-version: //p' core/daml.yaml)" = "$DAML_SDK_VERSION" \
    || { echo "core/daml.yaml sdk-version differs from DAML_SDK_VERSION $DAML_SDK_VERSION" >&2; exit 1; }
# Multi-package build: core (templates) then test (scripts, data-dep on core
# + the interface DARs). Produces core/.daml/dist/wormhole-core-<version>.dar and
# test/.daml/dist/wormhole-core-test-0.1.0.dar (which packs core's DALFs).
RUN dpm build --all
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

FROM dpm-sdk AS canton
WORKDIR /canton
# The production (clean) core DAR, for reference, plus the test DAR — which packs
# core's DALFs and the setup/integration scripts. The bootstrap uploads the test
# DAR (bundling core) and runs Test.TestCore:setup against the devnet sandbox.
# Fresh build stage: the glob matches exactly the version in core/daml.yaml.
COPY --from=build /canton-build/core/.daml/dist/wormhole-core-*.dar /canton/wormhole-core.dar
COPY --from=build /canton-build/test/.daml/dist/wormhole-core-test-0.1.0.dar /canton/wormhole-core-test.dar
COPY devnet/ /canton/devnet/
RUN chmod +x /canton/devnet/*.sh
# Ledger API v2 (gRPC).
EXPOSE 6865
CMD ["/canton/devnet/start_sandbox.sh"]
