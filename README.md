# WSL Agent Tool Execution Runtime

A Rust/Tokio workspace that separates a Windows agent from a Linux execution
runtime hosted in WSL.

## Architecture

```text
Windows Agent
    └── windows-agent-client
            │ wsl.exe bootstrap (start/discover)
            │ length-prefixed JSON over localhost TCP
            ▼
        persistent wsl-executor-daemon
            ├── one isolated session per connection/workspace
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
| `wsl-executor-daemon` | Persistent WSL loopback service, authentication and per-workspace sessions |
| `windows-agent-client` | Windows API, WSL service discovery and TCP transport |

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

Run or manage the persistent guest daemon inside WSL:

```bash
cargo run -p wsl-executor-daemon -- ensure-running
cargo run -p wsl-executor-daemon -- status
cargo run -p wsl-executor-daemon -- stop
```

`stdio` remains available for compatibility and protocol tests. The TCP service
binds only to WSL loopback, stores its port, PID, protocol version and random
authentication token in `~/.local/state/ate/daemon.json` (mode 0600), and uses
a file lock to serialize concurrent starts.

## Deploying the guest daemon in WSL

The Windows client uses a short `wsl.exe ... ensure-running` call to start or
discover the daemon, then keeps a TCP connection open for the workspace session.

Run `install-wsl-executor.ps1` from Windows; it handles everything:

```powershell
.\install-wsl-executor.ps1                     # auto-selects build strategy
.\install-wsl-executor.ps1 -BuildMode cross    # build from Windows, no Rust in WSL
.\install-wsl-executor.ps1 -InstallConfig      # also install a default config
.\install-wsl-executor.ps1 -InstallConfig -WorkspaceRoot /home/me/code
```

Two build strategies are supported, selected automatically (`-BuildMode auto`):

- **native** – compiles inside WSL using the distribution's own cargo. Needs a
  Rust toolchain in WSL; builds with `RUSTUP_TOOLCHAIN=stable` so the repo's
  `rust-toolchain.toml` pin is not forced into WSL.
- **cross** – cross-compiles a static musl Linux binary from Windows with
  `cargo-zigbuild` and copies it in. Requires no Rust inside WSL at all. Use
  `-CrossTarget gnu` for a dynamically-linked glibc binary instead.

Manually (native build, assumes you are already in WSL):

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

Manage the installed service:

```bash
~/.local/bin/ate-daemon ensure-running
~/.local/bin/ate-daemon status
~/.local/bin/ate-daemon stop
```

On the Windows side, connect with:

```rust
use windows_agent_client::{WslClient, WslClientConfig};

let client = WslClient::connect(WslClientConfig {
    distribution: "Ubuntu".into(),
    guest_program: "/home/<user>/.local/bin/ate-daemon".into(),
    workspace: "/home/<user>/code/my-project".into(),
})
.await?;
```

The workspace must be an existing absolute Linux path under `workspace_root`.
The daemon canonicalizes it and rejects `/mnt`, DrvFS/9P, and symlink escapes.
Each connection gets its own executor, cancellation map and file-read state;
the configured global concurrency limit is shared across all sessions. Shell
working directories are independently canonicalized before process spawn and
cannot escape the session workspace. Projects, Git data, dependencies and build
outputs remain on WSL ext4; no file synchronization protocol is involved.
