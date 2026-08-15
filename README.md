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

## Deploying the guest daemon in WSL

The daemon is launched per connection by the Windows client, so installing it
means placing the binary at a fixed path and pointing the client at it.

Run `install-wsl-executor.ps1` from Windows; it handles everything:

```powershell
.\install-wsl-executor.ps1                     # auto-selects build strategy
.\install-wsl-executor.ps1 -BuildMode cross    # build from Windows, no Rust in WSL
.\install-wsl-executor.ps1 -InstallConfig      # also install a default config
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
