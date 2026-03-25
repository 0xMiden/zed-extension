# Miden Extension for Zed

Zed extension for Miden Assembly based on `tree-sitter-masm` and `miden-lsp`.

## Current Assumptions

- `miden-lsp` is already available via `PATH`, or configured through Zed's
  `lsp.binary.path` override for `miden-lsp`
- grammar metadata is pinned to the same `tree-sitter-masm` lineage used by this repo

## Local Development

Install the extension in Zed with `Install Dev Extension` from the Extensions page,
or by running the `zed: install dev extension` action and selecting this directory.

## Layout

- `extension.toml`: Zed extension metadata, grammar registration, and language-server registration
- `src/lib.rs`: Zed extension entrypoint and `miden-lsp` launcher
- `languages/masm/`: Zed language config and tree-sitter query files
