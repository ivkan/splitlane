#!/usr/bin/env python3
"""Send one JSON-RPC 2.0 request to a running Splitlane and print the result.

    rpc.py <socket> <method> [json-params]

The `splitlane` CLI covers most of the choreography; this exists for the two
methods it has no verb for (`workspace.restore_layout`, `surface.list`'s full
envelope). The server reads newline-delimited JSON.
"""

import json
import socket
import sys


def main() -> int:
    if len(sys.argv) not in (3, 4):
        print(__doc__, file=sys.stderr)
        return 2
    path, method = sys.argv[1], sys.argv[2]
    params = json.loads(sys.argv[3]) if len(sys.argv) == 4 else {}
    request = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
        conn.settimeout(15)
        conn.connect(path)
        conn.sendall((json.dumps(request) + "\n").encode())
        buffer = b""
        while not buffer.endswith(b"\n"):
            chunk = conn.recv(65536)
            if not chunk:
                break
            buffer += chunk
    reply = json.loads(buffer)
    if "error" in reply:
        print(json.dumps(reply["error"]), file=sys.stderr)
        return 1
    print(json.dumps(reply.get("result"), indent=2))
    result = reply.get("result")
    return 1 if isinstance(result, dict) and "error" in result else 0


if __name__ == "__main__":
    sys.exit(main())
