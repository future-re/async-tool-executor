#!/usr/bin/env python3
import json
import struct
import sys


def read_frame():
    header = sys.stdin.buffer.read(4)
    if not header:
        return None
    length = struct.unpack(">I", header)[0]
    payload = sys.stdin.buffer.read(length)
    if len(payload) != length:
        raise EOFError("incomplete frame")
    return json.loads(payload)


def write_frame(message):
    payload = json.dumps(message, separators=(",", ":")).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(payload)) + payload)
    sys.stdout.buffer.flush()


while True:
    request = read_frame()
    if request is None:
        break
    kind = request.get("type")
    if kind == "hello":
        write_frame({"type": "hello_ack", "protocol_version": 1})
    elif kind == "invoke":
        write_frame({
            "type": "completed",
            "invocation_id": request["invocation_id"],
            "content": request["arguments"].get("value"),
        })
    elif kind == "cancel":
        write_frame({
            "type": "failed",
            "invocation_id": request["invocation_id"],
            "code": "cancelled",
            "message": "cancelled by host",
        })
    elif kind == "shutdown":
        write_frame({"type": "shutdown_ack"})
        break
