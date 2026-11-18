"""JSON Ledger API stub for emitter_id_scan_test.sh. Serves one fixture file on 127.0.0.1.

Usage: python3 -I emitter_id_scan_mock_api.py <fixture.json> <port-file>
The fixture holds: rights, ledgerEnd, updates (each with an offset), activeContracts, delaySeconds,
participantId, vettedPackages.
Each response waits delaySeconds first.
The server writes its port to <port-file> when it listens.
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

# Canton 3.7.3 rejects /v2/package-vetting/list with pageSize above 100 (HTTP 400).
CANTON_MAX_VETTING_PAGE_SIZE = 100

MAX_REQUEST_BYTES = 65536


def load_fixture(fixture_path):
    with open(fixture_path, encoding="utf-8") as fixture_file:
        return json.load(fixture_file)


def make_handler(fixture):
    class LedgerApiStub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def send_json(self, body):
            time.sleep(fixture["delaySeconds"])
            encoded = json.dumps(body).encode()
            try:
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)
            except (BrokenPipeError, ConnectionResetError):
                # The client timed out first.
                pass

        def read_request_json(self):
            length = int(self.headers.get("Content-Length", "0"))
            assert 0 <= length <= MAX_REQUEST_BYTES, "request body too large"
            return json.loads(self.rfile.read(length))

        def do_GET(self):
            if self.path == "/v2/authenticated-user":
                self.send_json({"user": {"id": "scan-user"}})
            elif self.path == "/v2/users/scan-user/rights":
                self.send_json({"rights": fixture["rights"]})
            elif self.path == "/v2/state/ledger-end":
                self.send_json({"offset": fixture["ledgerEnd"]})
            elif self.path == "/v2/parties/participant-id":
                self.send_json({"participantId": fixture["participantId"]})
            else:
                self.send_error(404)

        def do_POST(self):
            request = self.read_request_json()
            if self.path.startswith("/v2/updates?limit="):
                page_limit = int(self.path.split("=", 1)[1])
                begin = request["beginExclusive"]
                end = request["endInclusive"]
                page = [
                    {"update": {"Transaction": {"value": update}}}
                    for update in fixture["updates"]
                    if begin < update["offset"] <= end
                ][:page_limit]
                self.send_json(page)
            elif self.path == "/v2/package-vetting/list":
                if not 1 <= request["pageSize"] <= CANTON_MAX_VETTING_PAGE_SIZE:
                    self.send_error(400, "pageSize out of range")
                    return
                expected_filters = (
                    request["packageMetadataFilter"]["packageNamePrefixes"] == ["wormhole-core"]
                    and request["topologyStateFilter"]["participantIds"] == [fixture["participantId"]]
                )
                if not expected_filters:
                    self.send_error(400, "unexpected package-vetting filter")
                    return
                self.send_json(
                    {
                        "vettedPackages": [
                            {
                                "packages": fixture["vettedPackages"],
                                "participantId": fixture["participantId"],
                                "synchronizerId": "synchronizer::1220aa",
                                "topologySerial": 1,
                            }
                        ],
                        "nextPageToken": "",
                    }
                )
            elif self.path == "/v2/state/active-contracts":
                self.send_json(
                    [
                        {"contractEntry": {"JsActiveContract": {"createdEvent": created_event}}}
                        for created_event in fixture["activeContracts"]
                    ]
                )
            else:
                self.send_error(404)

    return LedgerApiStub


def main():
    fixture_path, port_file = sys.argv[1], sys.argv[2]
    server = HTTPServer(("127.0.0.1", 0), make_handler(load_fixture(fixture_path)))
    with open(port_file, "w", encoding="utf-8") as port_output:
        port_output.write(str(server.server_address[1]))
    # Runs until the test sends SIGTERM.
    server.serve_forever()


if __name__ == "__main__":
    main()
