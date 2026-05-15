# Miden Extension for Zed

Zed extension for Miden Assembly. Bundles:

- **Language support** via `tree-sitter-masm` grammar and `miden-lsp` (semantic
  tokens, completions, diagnostics).
- **Debugger** (DAP client) that talks to a small stdio proxy. The proxy connects
  to the Miden VM debug adapter served by `miden-client exec --start-debug-adapter`
  or standalone `miden-debug` over TCP.

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
a proxy. The proxy connects to a TCP DAP server. It can attach to an existing
server, launch `miden-client` for transaction scripts, or launch `miden-debug`
directly for standalone `.masm` / `.masp` programs.

### Backend prerequisites

Until the upstream crates release the fixes this extension depends on,
transaction debug sessions need Python 3 plus `miden-client` built from the
companion feature branches. Standalone sessions need `miden-debug` from the
matching debugger branch.

| Repo | Branch | Fix |
| ---- | ------ | --- |
| [walnuthq/miden-client](https://github.com/walnuthq/miden-client/tree/feature/vscode-dap-plugin) | `feature/vscode-dap-plugin` | `compile_tx_script` takes the script path so DAP clients receive real `Source.path` + line numbers for user code. |
| [0xMiden/miden-debug](https://github.com/0xMiden/miden-debug/tree/feature/vscode-dap-plugin) | `feature/vscode-dap-plugin` | DAP server handles `Command::Attach` instead of rejecting it with "Unsupported command". |

The `miden-client` branch already carries a `[patch.crates-io]` entry that
pins `miden-debug` to the matching branch, so a plain
`cargo build --bin miden-client --features testing` is enough.

### Attach mode

Start a DAP server yourself, then attach from Zed. Zed spawns a Python stdio
proxy, and the proxy connects to the TCP server. For a transaction:

```bash
miden-client exec \
  --script-path examples/test_debug.masm \
  --start-debug-adapter 127.0.0.1:4711
```

For a standalone program:

```bash
miden-debug --start-debug-adapter 127.0.0.1:4711 examples/simple.masm
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

### Transaction Launch

Zed spawns the Python stdio proxy, and the proxy starts `miden-client` and
connects to it over TCP:

```json
[
  {
    "label": "Debug Miden Script",
    "adapter": "miden",
    "config": {
      "request": "launch",
      "runtime": "client",
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

### Standalone Launch

Standalone launch means Zed bypasses `miden-client` completely. Zed starts the
Python stdio proxy, the proxy starts `miden-debug`, and then connects to
`miden-debug` over TCP. This mode debugs a plain Miden VM program: either a
`.masm` source file or a compiled `.masp` package.

Use this mode for:

- simple MASM scripts that do not need a transaction context
- compiled Rust/Miden compiler examples such as `target/miden/debug/*.masp`
- quick source-level debugger checks where accounts, notes, transaction
  kernels, and the client store are not involved

Do not use standalone launch when you want to debug real transaction execution.
For that, use `runtime: "client"` so Zed starts `miden-client exec`.

The proxy starts roughly this command and then connects Zed to it:

```bash
miden-debug --start-debug-adapter 127.0.0.1:4711 <programPath> -- <programArgs>
```

```json
[
  {
    "label": "Debug Miden Program",
    "adapter": "miden",
    "config": {
      "request": "launch",
      "runtime": "debugger",
      "programPath": "${ZED_FILE}",
      "midenDebugPath": "miden-debug",
      "host": "127.0.0.1",
      "port": 4711
    }
  }
]
```

`programPath` can point to a standalone `.masm` source file or compiled `.masp`
package. `programArgs` become the operand stack arguments after `--`.

For compiler-generated packages, especially Rust examples, the configuration
usually needs the same values you would pass to `miden-debug` on the command
line:

```json
[
  {
    "label": "Debug Fibonacci Package",
    "adapter": "miden",
    "config": {
      "request": "launch",
      "runtime": "debugger",
      "programPath": "/path/to/project/target/miden/debug/fibonacci.masp",
      "midenDebugPath": "miden-debug",
      "sysroot": "/path/to/miden-core-lib/assets",
      "programArgs": ["10"]
    }
  }
]
```

Use `inputsPath`, `entrypoint`, `sysroot`, `searchPath`, and `linkLibraries`
for the corresponding `miden-debug` CLI options. For compiled packages built
with `midenc -Ztrim-path-prefix=...`, set `trimPathPrefixes`,
`sourcePathPrefixes`, or `compilerArgs`; packages under
`target/miden/{debug,release}` infer the package root automatically.

Debugger state is provided by the `miden-debug` DAP server as three scopes:
`Local Variables`, `Operand Stack`, and `Memory`. The Zed proxy forwards DAP
`scopes` and `variables` responses unchanged, so if a scope is missing, check
that `midenDebugPath` points at a recently rebuilt `miden-debug` binary.

### Configuration reference

| Property          | Type     | Default        | Mode       | Description                                      |
| ----------------- | -------- | -------------- | ---------- | ------------------------------------------------ |
| `request`         | string   | —              | all        | `"launch"` or `"attach"`                         |
| `runtime`         | string   | `client`       | launch     | `client` for transaction debugging, `debugger` for standalone `miden-debug` |
| `host`            | string   | `127.0.0.1`    | all        | DAP server host                                  |
| `port`            | integer  | `4711`         | all        | DAP server port                                  |
| `scriptPath`      | string   | —              | client     | Transaction `.masm` script                       |
| `programPath`     | string   | —              | debugger   | Standalone `.masm` source or `.masp` package     |
| `midenClientPath` | string   | `miden-client` | client     | Path to the `miden-client` binary                |
| `midenDebugPath`  | string   | `miden-debug`  | debugger   | Path to the `miden-debug` binary                 |
| `pythonPath`      | string   | `python3`      | all        | Path to Python 3 for the stdio proxy             |
| `accountId`       | string   | —              | client     | Account ID for transaction execution             |
| `inputsPath`      | string   | —              | debugger   | Path to a miden-debug inputs TOML file           |
| `programArgs`     | string[] | `[]`           | debugger   | Operand stack arguments                          |
| `entrypoint`      | string   | —              | debugger   | Entrypoint for library packages                  |
| `sysroot`         | string   | —              | debugger   | Miden sysroot                                    |
| `searchPath`      | string[] | `[]`           | debugger   | Extra library search paths                       |
| `linkLibraries`   | string[] | `[]`           | debugger   | Libraries passed with `--link-library`           |
| `sourcePathPrefixes` | string[] | `[]`       | debugger   | Explicit source path prefixes for source mapping |
| `trimPathPrefixes` | string[] | `[]`          | debugger   | `-Ztrim-path-prefix` values for source mapping   |
| `compilerArgs`    | string[] | `[]`           | debugger   | Args scanned for `-Ztrim-path-prefix=...`        |
| `midencArgs`      | string[] | `[]`           | debugger   | Alias args scanned for `-Ztrim-path-prefix=...`  |
| `buildArgs`       | string[] | `[]`           | debugger   | Alias args scanned for `-Ztrim-path-prefix=...`  |
| `cargoMidenArgs`  | string[] | `[]`           | debugger   | Alias args scanned for `-Ztrim-path-prefix=...`  |
| `cwd`             | string   | worktree root  | launch     | Working directory for the launched backend       |
