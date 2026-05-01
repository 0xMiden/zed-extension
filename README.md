# Miden Extension for Zed

Zed extension for Miden Assembly. Bundles:

- **Language support** via `tree-sitter-masm` grammar and `miden-lsp` (semantic
  tokens, completions, diagnostics).
- **Debugger** (DAP client) that talks to a small stdio proxy. The proxy connects
  to the Miden VM debug adapter served by `miden-client exec --start-debug-adapter`
  over TCP.

## Current Assumptions

- `miden-lsp` is already available via `PATH`, or configured through Zed's
  `lsp.binary.path` override for `miden-lsp`
- grammar metadata is pinned to the same `tree-sitter-masm` lineage used by this repo

## Local Development

Install the extension in Zed with `Install Dev Extension` from the Extensions page,
or by running the `zed: install dev extension` action and selecting this directory.

## Layout

- `extension.toml`: Zed extension metadata, grammar registration, language-server and debug-adapter registration
- `src/lib.rs`: Zed extension entrypoint, `miden-lsp` launcher, DAP adapter binary builder
- `dap/schema.json`: JSON Schema describing the debug-configuration shape (Zed UI)
- `languages/masm/`: Zed language config and tree-sitter query files

## Debugging

The extension contributes a `miden` debug adapter that speaks DAP over stdio to
a proxy. The proxy connects to the TCP server exposed by `miden-client`. Two
modes are supported.

### Backend prerequisites

Until the upstream crates release the fixes this extension depends on, debug
sessions need Python 3 plus `miden-client` built from the companion feature
branches:

| Repo | Branch | Fix |
| ---- | ------ | --- |
| [walnuthq/miden-client](https://github.com/walnuthq/miden-client/tree/feature/vscode-dap-plugin) | `feature/vscode-dap-plugin` | `compile_tx_script` takes the script path so DAP clients receive real `Source.path` + line numbers for user code. |
| [0xMiden/miden-debug](https://github.com/0xMiden/miden-debug/tree/feature/vscode-dap-plugin) | `feature/vscode-dap-plugin` | DAP server handles `Command::Attach` instead of rejecting it with "Unsupported command". |

The `miden-client` branch already carries a `[patch.crates-io]` entry that
pins `miden-debug` to the matching branch, so a plain
`cargo build --bin miden-client --features testing` is enough.

### Attach mode

Start the DAP server yourself, then attach from Zed. Zed spawns a Python stdio
proxy, and the proxy connects to the TCP server. In a terminal:

```bash
miden-client exec \
  --script-path examples/test_debug.masm \
  --start-debug-adapter 127.0.0.1:4711
```

In `.zed/debug.json`:

```json
[
  {
    "label": "Attach to Miden DAP",
    "adapter": "miden",
    "config": {
      "request": "attach",
      "host": "127.0.0.1",
      "port": 4711
    }
  }
]
```

### Launch mode

Zed spawns the Python stdio proxy, and the proxy starts `miden-client` and
connects to it over TCP:

```json
[
  {
    "label": "Debug Miden Script",
    "adapter": "miden",
    "config": {
      "request": "launch",
      "scriptPath": "${ZED_FILE}",
      "midenClientPath": "miden-client",
      "host": "127.0.0.1",
      "port": 4711
    }
  }
]
```

`cwd` (optional) must point at an initialized `miden-client` working directory.
When omitted it defaults to the worktree root.

### Configuration reference

| Property          | Type    | Default        | Mode   | Description                                        |
| ----------------- | ------- | -------------- | ------ | -------------------------------------------------- |
| `request`         | string  | —              | both   | `"launch"` or `"attach"`                           |
| `host`            | string  | `127.0.0.1`    | both   | DAP server host                                    |
| `port`            | integer | `4711`         | both   | DAP server port                                    |
| `scriptPath`      | string  | —              | launch | Absolute path to the `.masm` script                |
| `midenClientPath` | string  | `miden-client` | launch | Path to the `miden-client` binary                  |
| `pythonPath`      | string  | `python3`      | both   | Path to Python 3 for the stdio proxy               |
| `accountId`       | string  | —              | launch | Account ID for execution (optional)                |
| `cwd`             | string  | worktree root  | launch | Working directory (initialized miden-client dir)   |
