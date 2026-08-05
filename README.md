# WSL Agent Tool Execution Runtime

A Rust/Tokio workspace that separates a Windows agent from a Linux execution
runtime hosted in WSL.

## Architecture

```text
Windows Agent
    └── windows-agent-client
            │ length-prefixed JSON over wsl.exe stdio
            ▼
        wsl-executor-daemon
            ├── executor-protocol
            ├── executor-core
            └── wsl-runtime
                    └── isolated Linux child processes
```

| Crate | Responsibility |
| --- | --- |
| `executor-protocol` | Versioned, platform-neutral host/guest messages and framing |
| `executor-core` | Tool registry, scheduling, concurrency, timeout, cancellation and observation |
| `wsl-runtime` | Linux process execution, resource limits and process-group cleanup |
| `wsl-executor-daemon` | WSL stdio server, handshake and active-task lifecycle |
| `windows-agent-client` | Windows API and `wsl.exe` process transport |

The Windows side submits tool requests. Tool resolution, scheduling and process
execution remain on the WSL side, so the Windows client does not act as a remote
`Shell` implementation.

## Development

```bash
cargo test --workspace
```

Run the guest daemon inside WSL with framed protocol messages on stdin/stdout:

```bash
cargo run -p wsl-executor-daemon
```

The protocol currently supports handshake, execute, cancel, tool discovery,
progress events, terminal results and graceful shutdown.
