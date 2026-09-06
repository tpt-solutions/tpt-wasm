# WebAssembly Proposals

This document tracks the implementation status of WebAssembly proposals.
The machine-readable registry is in `feature-registry.toml`.

## Implementation ladder

For each proposal, track independently:

| Stage | Description |
|---|---|
| decoded | Binary format understood by `tpt-wasm-decode` |
| validated | Validation rules in `tpt-wasm-validate` |
| executed | Micro Interpreter executes it |
| compiled | Compiler generates code for it |
| tested | Spec tests pass |
| fuzzed | Fuzz target exists |
| differential | Differential test exists |
| verified | Formal property established |

A proposal is not "supported" until all stages are complete.

## Proposal resources

- Proposals overview: https://github.com/WebAssembly/proposals
- Phase definitions: https://github.com/WebAssembly/meetings/blob/main/process/phases.md
