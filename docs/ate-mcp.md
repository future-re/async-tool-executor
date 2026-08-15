# ate-mcp: expose the WSL executor to an MCP agent

`ate-mcp` is a Model Context Protocol (MCP) **stdio server** that bridges an MCP
client (for example opencode) to the persistent WSL executor daemon. Every tool
registered on the daemon (`Read`, `Write`, `Edit`, `Glob`, `Grep`, `WebFetch`,
`shell`, and any installed plugins) becomes an MCP tool the agent can call, so
the agent runs commands and reads files on WSL ext4 instead of only on Windows.

```text
MCP client (opencode)
    │  JSON-RPC 2.0 over newline-delimited stdio
    ▼
ate-mcp.exe
    │  windows-agent-client (wsl.exe bootstrap + localhost TCP)
    ▼
wsl-executor-daemon (persistent, in WSL)
    └── executor-core / executor-tools / wsl-runtime
```

## Requirements

- Windows with WSL2 and a Linux distribution installed and running.
- The guest daemon deployed in WSL (see the top-level `README.md`,
  "Deploying the guest daemon in WSL").
- A Rust toolchain on Windows (`rustup` + stable).
- A working directory in WSL that exists and is inside the daemon's
  `workspace_root`.

## Building

From the workspace root, on Windows:

```powershell
cargo build --release -p ate-mcp
```

The binary is produced at `target\release\ate-mcp.exe`. Nothing is installed
globally; copy it anywhere you like and point the MCP configuration at it.

## One-command install

`install-wsl-executor.ps1` builds the daemon, deploys it to WSL, and installs
both Windows binaries (`ate.exe` and `ate-mcp.exe`) in one run. Replace the
placeholders with your own values:

```powershell
.\install-wsl-executor.ps1 -Distribution <distro> -InstallConfig -WorkspaceRoot /home/<user>/code
```

## Distributing via GitHub Releases

The WSL daemon is a static Linux binary, so it can be shipped as a Release
asset and installed without any compiler. Pushing a tag like `v0.1.0` triggers
the CI workflow in `.github/workflows/release.yml`, which cross-compiles the
daemon for **both** `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` on GitHub's runners, packages each as
`ate-daemon-<tag>-<target>.tar.gz`, and uploads both to the Release. The
correct one is selected automatically at install time from the WSL
architecture.

CI handles the build; no local compiler is needed:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

Alternatively, `publish-release.ps1` cross-compiles and publishes from a
Windows machine (requires `cargo`, `cargo-zigbuild`, `zig` and the GitHub
CLI):

```powershell
.\publish-release.ps1 -Tag v0.1.0 -InstallTools
```

Install the prebuilt daemon for the local WSL architecture. The `{arch}`
placeholder is replaced automatically, so one URL serves every platform:

```powershell
.\install-wsl-executor.ps1 -ReleaseUrl "https://github.com/<owner>/<repo>/releases/download/v0.1.0/ate-daemon-v0.1.0-{arch}.tar.gz" -InstallConfig -WorkspaceRoot /home/<user>/code
```

Or let the script build the URL from the repository and tag:

```powershell
.\install-wsl-executor.ps1 -Repo <owner>/<repo> -ReleaseTag v0.1.0 -InstallConfig -WorkspaceRoot /home/<user>/code
```

The `-ReleaseUrl` / `-Repo` forms need no Rust toolchain, no `cargo-zigbuild`,
and no `zig` on the target machine. The script detects the WSL architecture
via `uname -m` and maps it to a Rust target (`x86_64` → musl x86_64,
`aarch64`/`arm64` → musl aarch64).

## Configuration

`ate-mcp` reads its connection settings from environment variables:

| Variable | Required | Meaning | Default |
| --- | --- | --- | --- |
| `ATE_MCP_DISTRIBUTION` | no | WSL distribution name | `Ubuntu` |
| `ATE_MCP_GUEST_PROGRAM` | **yes** | Absolute path of the daemon inside WSL | — |
| `ATE_MCP_WORKSPACE` | **yes** | Existing absolute WSL path the session runs in | — |

`ATE_MCP_GUEST_PROGRAM` must be an absolute Linux path (for example
`~/.local/bin/ate-daemon`). `ATE_MCP_WORKSPACE` must exist and stay under the
daemon's `workspace_root`; it is canonicalized and checked for `/mnt`, DrvFS/9P
and symlink escapes.

The server keeps one connection to the daemon for its whole lifetime. It
implements the MCP `initialize`, `tools/list`, `tools/call` and `ping`
methods. `tools/call` forwards to the daemon and returns the tool's output
text; for file tools such as `Read` the file body is surfaced directly.

## Using with opencode

Add an MCP entry to the project's `opencode.json` (or the user/global config),
substituting your own values for the placeholders:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "ate-wsl": {
      "type": "local",
      "command": ["C:\\path\\to\\target\\release\\ate-mcp.exe"],
      "environment": {
        "ATE_MCP_DISTRIBUTION": "Ubuntu",
        "ATE_MCP_GUEST_PROGRAM": "/home/<user>/.local/bin/ate-daemon",
        "ATE_MCP_WORKSPACE": "/home/<user>/<workspace>"
      }
    }
  }
}
```

Replace:

- `C:\path\to\...\ate-mcp.exe` — the absolute path of the built binary.
- `ATE_MCP_DISTRIBUTION` — your WSL distribution name.
- `ATE_MCP_GUEST_PROGRAM` — where the daemon was installed in WSL.
- `ATE_MCP_WORKSPACE` — the WSL directory you want the agent to operate in.

Config is loaded once when opencode starts, so **quit and restart opencode**
after editing `opencode.json`. The daemon's tools then appear in the agent's
tool list under the `ate-wsl` prefix (for example `ate-wsl_shell`,
`ate-wsl_Read`). Tool arguments use WSL absolute paths, such as
`/home/<user>/<workspace>/README.md`.

## Using with any MCP client

Point any MCP stdio client at the binary and supply the three environment
variables. The protocol is standard MCP, so no client-specific code is needed.

## Troubleshooting

- **`ate-mcp: environment variable not found`** — `ATE_MCP_GUEST_PROGRAM` or
  `ATE_MCP_WORKSPACE` is missing. Set both.
- **`failed to start WSL executor`** — the `wsl.exe` bootstrap call failed;
  confirm the distribution name and daemon path, and that WSL is running.
- **`guest rejected the request (invalid_workspace)`** — `ATE_MCP_WORKSPACE`
  does not exist, is not absolute, or is outside the daemon's `workspace_root`.
- **Tool calls return instantly / no tools listed** — the daemon is not running
  or the cached endpoint is stale; `ate-mcp` rediscovers it on the next
  connection attempt.
