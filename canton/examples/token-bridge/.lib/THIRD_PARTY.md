# Vendored third-party DARs (Canton Network Token Standard)

These DARs implement the Canton Network Token Standard (CIP-0056) and are required
to build the token-bridge example. They are vendored (committed) here; do not
regenerate them by an unaudited fetch.

## Provenance

- Source: [`digital-asset/cn-quickstart`](https://github.com/digital-asset/cn-quickstart/tree/main/quickstart/daml/dars),
  which mirrors the Splice release bundle v0.5.18 (Canton 3.4 / Daml SDK 3.4.11).
- sha256s verified against the upstream Splice release bundle:
  `https://github.com/digital-asset/decentralized-canton-sync/releases/download/v0.5.18/0.5.18_splice-node.tar.gz`
  (bundle sha256: `4639977f854f3c3c032d7dbe93a933f74644bd410a128f1e7d5371bc988653e1`)

## Pinned files (sha256)

| File | sha256 |
| --- | --- |
| splice-api-token-metadata-v1-1.0.0.dar | 455eb160cb5abd4ae9918a6fbb9dad471f721adda39f0e5c76feef08d05637fc |
| splice-api-token-holding-v1-1.0.0.dar | ef75f8eb41a65810221784fdb78bb9dfac7cb22245aba14fa7cb7f69c34e0175 |
| splice-api-token-transfer-instruction-v1-1.0.0.dar | e4c73aa7ae73fb2fc330b938ffb99f568792321640ba4b9472902aa8d742c994 |

Package versions are all `1.0.0`; built with Daml SDK 3.3.x (Daml-LF 2.x), which is
data-dependency compatible with this project's SDK 3.4.11.

## Re-fetch (only with authorization)

    curl -fSL -o splice-api-token-metadata-v1-1.0.0.dar \
      https://raw.githubusercontent.com/digital-asset/cn-quickstart/main/quickstart/daml/dars/splice-api-token-metadata-v1-1.0.0.dar
    curl -fSL -o splice-api-token-holding-v1-1.0.0.dar \
      https://raw.githubusercontent.com/digital-asset/cn-quickstart/main/quickstart/daml/dars/splice-api-token-holding-v1-1.0.0.dar
    curl -fSL -o splice-api-token-transfer-instruction-v1-1.0.0.dar \
      https://raw.githubusercontent.com/digital-asset/cn-quickstart/main/quickstart/daml/dars/splice-api-token-transfer-instruction-v1-1.0.0.dar
    # verify each DAR sha256 above
