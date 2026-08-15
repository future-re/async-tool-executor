# ATE Plugin Protocol v1

Plugins are trusted local programs. The daemon starts one persistent process per
package on its first invocation, sets its working directory to the package root,
and reserves stdout exclusively for protocol frames. Diagnostics belong on
stderr.

Each frame is a four-byte big-endian unsigned payload length followed by one
UTF-8 JSON object. Frames larger than 8 MiB are rejected.

## Handshake

The daemon first sends:

```json
{"type":"hello","protocol_version":1,"package_id":"com.example.echo"}
```

The plugin must answer within the configured startup timeout:

```json
{"type":"hello_ack","protocol_version":1}
```

## Invocation

Calls may be concurrent and responses may be out of order. Every call-related
message is correlated by `invocation_id`.

```json
{"type":"invoke","invocation_id":"call-1","tool":"example_echo","arguments":{"value":"hello"},"cwd":"/workspace","timeout_ms":30000}
{"type":"progress","invocation_id":"call-1","message":"working","data":null}
{"type":"completed","invocation_id":"call-1","content":"hello"}
```

A failed call uses a stable plugin-defined code and human-readable message:

```json
{"type":"failed","invocation_id":"call-1","code":"invalid_input","message":"value is required"}
```

On cancellation the daemon sends `{"type":"cancel","invocation_id":"call-1"}`.
The plugin must finish that invocation with `completed` or `failed`; otherwise
the daemon kills the entire package process after the cancellation grace period.
This also fails other calls currently running in that process. Failed calls are
never replayed automatically.

Before normal connection shutdown the daemon may send `{"type":"shutdown"}`;
the plugin should answer `{"type":"shutdown_ack"}` and exit. EOF always means
the host connection is gone and the plugin should terminate.
