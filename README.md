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
| `executor-tools` | Platform-neutral base tools: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `WebFetch` |
| `wsl-runtime` | Linux process execution, resource limits and process-group cleanup |
| `wsl-executor-daemon` | WSL stdio server, handshake and active-task lifecycle |
| `windows-agent-client` | Windows API and `wsl.exe` process transport |

The Windows side submits tool requests. Tool resolution, scheduling and process
execution remain on the WSL side, so the Windows client does not act as a remote
`Shell` implementation.

## Registering tools

Any crate in the workspace can add a tool to the daemon:

1. Implement [`executor_core::Tool`](crates/executor-core/src/tool.rs) — provide
   `definition()` (name, description, JSON input schema) and `invoke()`.
   `validate()`, `is_concurrency_safe()` and `invocation_detail()` have sensible
   defaults.
2. Call `ToolRegistry::register(...)` before building the `ToolExecutor`.

The built-in set lives in [`executor-tools`](crates/executor-tools), which also
exposes `register_core_tools(&mut registry)` to register all six base tools
(`Read`, `Write`, `Edit`, `Glob`, `Grep`, `WebFetch`) at once. The daemon wires
them together with the Linux `shell` tool:

```rust
use executor_core::{ExecutorConfig, ToolExecutor, ToolRegistry};
use executor_tools::register_core_tools;

let mut registry = ToolRegistry::new();
register_core_tools(&mut registry);
registry.register(ShellTool::new(shell));

let executor = ToolExecutor::new(registry, ExecutorConfig::default());
```

Registered tools appear in `ToolRegistry::list()`, which the daemon serves over
`ListTools` capability discovery.

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

## Deploying the guest daemon in WSL

The daemon is launched per connection by the Windows client, so installing it
means placing the binary at a fixed path and pointing the client at it.

Build a release binary and install it (assumes you are already in WSL):

```bash
cargo build --release -p wsl-executor-daemon
mkdir -p ~/.local/bin
cp target/release/wsl-executor-daemon ~/.local/bin/ate-daemon
chmod +x ~/.local/bin/ate-daemon
```

Configuration is loaded from the first available source:

1. `--config <path>` command-line argument
2. the `ATE_CONFIG` environment variable
3. `~/.config/ate/config.json`
4. built-in defaults

A missing file falls back to defaults; a file that exists but is malformed is a
hard error. See `crates/wsl-executor-daemon/config.example.json` for every
supported field.

Smoke-test the installed binary against a real protocol session:

```bash
~/.local/bin/ate-daemon < /dev/null   # exits cleanly on EOF, validating startup
```

On the Windows side, connect with:

```rust
use windows_agent_client::{WslClient, WslClientConfig};

let client = WslClient::connect(WslClientConfig {
    distribution: "Ubuntu".into(),
    guest_program: "/home/<user>/.local/bin/ate-daemon".into(),
})
.await?;
```
