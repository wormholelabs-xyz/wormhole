# syntax=docker/dockerfile:1
#
# Canton devnet image for the Wormhole core bridge.
#
# Uses the Canton Network `dpm` toolchain (https://docs.canton.network/sdks-tools/cli-tools/dpm),
# not the legacy `daml` assistant. Stage 1 installs dpm + the SDK pinned in
# daml.yaml and builds the `wormhole-core` DAR; stage 2 runs a Canton sandbox
# (Ledger API v2 on :6865) with the DAR preloaded, and a companion bootstrap step
# (see devnet/canton-devnet.yaml) allocates the Operator party and creates the
# CoreState via Test.TestCore:setup.
#
# dpm manages the SDK from daml.yaml's `sdk-version` (Daml 3.5.1 → Canton 3.5.x
# protocol, Daml-LF 2.3 / Protocol Version 35; Ledger API v2 + DA.Crypto.Text
# builtins). Canton is JVM-based, so the base image ships a JDK
# (17+).
#
# NOTE: building this image needs network access to get.digitalasset.com (dpm
# installer) so it is not built in this repo's CI. The dpm commands here
# (`dpm install <version>`, `dpm build`) and the sandbox/script flags used by the
# devnet scripts have been validated against dpm for the pinned SDK.
ARG DAML_SDK_VERSION=3.5.1

FROM eclipse-temurin:17-jdk@sha256:91b6210cce02091f6f0798a83ec51aa223828242c5a21a85793bb8c28dc891c4 AS build
ARG DAML_SDK_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Install the dpm CLI, then the SDK version pinned in daml.yaml.
RUN curl -fsSL https://get.digitalasset.com/install/install.sh | sh
ENV PATH="/root/.dpm/bin:${PATH}"
WORKDIR /canton-build
COPY multi-package.yaml ./
# Vendored CIP-0056 token-standard interface DARs (core fees data-depend on
# them); must be present before the build.
COPY dars ./dars
COPY core ./core
COPY test ./test
# Multi-package build: core (templates) then test (scripts, data-dep on core
# + the interface DARs). Produces core/.daml/dist/wormhole-core-0.4.0.dar and
# test/.daml/dist/wormhole-core-test-0.1.0.dar (which packs core's DALFs).
RUN dpm install "${DAML_SDK_VERSION}" && dpm build --all

FROM eclipse-temurin:17-jdk@sha256:91b6210cce02091f6f0798a83ec51aa223828242c5a21a85793bb8c28dc891c4 AS canton
ARG DAML_SDK_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://get.digitalasset.com/install/install.sh | sh
ENV PATH="/root/.dpm/bin:${PATH}"
WORKDIR /canton
# The production (clean) core DAR, for reference, plus the test DAR — which packs
# core's DALFs and the setup/integration scripts. The bootstrap uploads the test
# DAR (bundling core) and runs Test.TestCore:setup against the devnet sandbox.
COPY --from=build /canton-build/core/.daml/dist/wormhole-core-0.4.0.dar /canton/wormhole-core.dar
COPY --from=build /canton-build/test/.daml/dist/wormhole-core-test-0.1.0.dar /canton/wormhole-core-test.dar
COPY devnet/ /canton/devnet/
# Pre-install the SDK so `dpm sandbox` / `dpm script` start without a fetch.
# `dpm install` requires an explicit version argument.
RUN dpm install "${DAML_SDK_VERSION}" && chmod +x /canton/devnet/*.sh
# Ledger API v2 (gRPC).
EXPOSE 6865
CMD ["/canton/devnet/start_sandbox.sh"]
