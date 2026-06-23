#!/usr/bin/env python3
"""Regenerate ntt_test_vectors.json from the wormchain dump + wormholescan.

The test (`tests/test_vectors.rs`) replays signed mainnet NTT VAAs through the
program's parser and checks them against what the wormchain NTT global-accountant
actually committed. This tool builds that corpus:

  1. Read a decoded catalogue of the wormchain NTT accountant dump. Each
     `transfer` row carries the committed `(chain, emitter, sequence)` key and
     the normalized result `(amount, token_chain, token_address, recipient_chain)`.
     `transceiver_hub` rows give the hub token-identity registry; the
     `relayer_chain_registration` rows say which emitter is the relayer per chain.
  2. For a sample of transfers, download the signed VAA from wormholescan.
  3. Mark each transfer relayer-delivered or direct (emitter == chain's relayer).
  4. Write the corpus: the hub registry plus one entry per fetched transfer.

The catalogue is produced by decoding the preserved dump
(`~/wormchain-migration-archive/ntt-raw-dump.jsonl`) with the wormchain-snapshot
tool's `--input` mode. Curl is used for HTTPS (portable cert handling).

Usage:
  fetch_test_vectors.py --catalogue catalogue.jsonl --out ntt_test_vectors.json
                        [--per-source 2]
"""

import argparse
import base64
import collections
import json
import subprocess

WORMHOLESCAN = "https://api.wormholescan.io/api/v1/vaas/{chain}/{emitter}/{seq}"


def curl_json(url: str) -> dict:
    out = subprocess.run(
        ["curl", "-sS", "--max-time", "25", "-H", "Accept: application/json", url],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return json.loads(out)


def vaa_first_payload_byte(vaa_b64: str) -> int:
    raw = base64.b64decode(vaa_b64)
    num_sigs = raw[5]
    body = raw[6 + 66 * num_sigs :]
    return body[51]  # payload byte 0 (after the 51-byte body header)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--catalogue", required=True, help="decoded NTT accountant catalogue.jsonl")
    ap.add_argument("--out", required=True, help="output corpus path")
    ap.add_argument(
        "--per-source",
        type=int,
        default=2,
        help="transfers to sample per distinct (chain, emitter) source",
    )
    args = ap.parse_args()

    relayers: dict[int, str] = {}
    hubs: list[dict] = []
    by_source: dict[tuple, list[dict]] = collections.defaultdict(list)
    for line in open(args.catalogue):
        rec = json.loads(line)
        kind = rec.get("kind")
        if kind == "relayer_chain_registration":
            relayers[rec["chain"]] = rec["registered_emitter"]
        elif kind == "transceiver_hub":
            hubs.append(
                {
                    "chain": rec["chain"],
                    "address": rec["address"],
                    "hub_chain": rec["hub_chain"],
                    "hub_address": rec["hub_address"],
                }
            )
        elif kind == "transfer":
            by_source[(rec["chain"], rec["emitter"])].append(rec)

    vectors: list[dict] = []
    for (chain, emitter), rows in sorted(by_source.items()):
        rows.sort(key=lambda r: r["sequence"])
        # Sample the lowest and highest sequences: low sequences tend to be
        # direct TransceiverMessages, high ones relayer-delivered.
        picks = rows[: max(1, args.per_source - 1)] + rows[-1:]
        seen = set()
        for r in picks:
            if r["sequence"] in seen:
                continue
            seen.add(r["sequence"])
            resp = curl_json(WORMHOLESCAN.format(chain=chain, emitter=emitter[2:], seq=r["sequence"]))
            vaa_b64 = resp.get("data", {}).get("vaa")
            if not vaa_b64:
                print(f"  skip chain={chain} seq={r['sequence']}: no VAA on wormholescan")
                continue
            via_relayer = relayers.get(chain) == emitter
            # Sanity cross-check: relayer VAAs lead with the DeliveryInstruction
            # payload id (0x01); direct ones with the TransceiverMessage prefix (0x99).
            first = vaa_first_payload_byte(vaa_b64)
            if via_relayer != (first == 0x01):
                raise SystemExit(
                    f"dispatch mismatch chain={chain} seq={r['sequence']}: "
                    f"via_relayer={via_relayer} but first payload byte=0x{first:02x}"
                )
            vectors.append(
                {
                    "chain": chain,
                    "emitter": emitter,
                    "sequence": r["sequence"],
                    "via_relayer": via_relayer,
                    "expected_digest": r["digest"],
                    "expected_amount": r["amount"],
                    "expected_token_chain": r["token_chain"],
                    "expected_token_address": r["token_address"],
                    "expected_recipient_chain": r["recipient_chain"],
                    "vaa_hex": "0x" + base64.b64decode(vaa_b64).hex(),
                }
            )

    vectors.sort(key=lambda v: (v["chain"], v["sequence"]))
    corpus = {
        "comment": (
            "Signed mainnet NTT VAAs (wormholescan) paired with what the "
            "wormchain NTT global-accountant committed. Regenerate with "
            "fetch_test_vectors.py."
        ),
        "hubs": sorted(hubs, key=lambda h: (h["chain"], h["address"])),
        "vectors": vectors,
    }
    with open(args.out, "w") as f:
        json.dump(corpus, f, indent=1)
    print(f"wrote {len(vectors)} vectors, {len(hubs)} hubs -> {args.out}")


if __name__ == "__main__":
    main()
