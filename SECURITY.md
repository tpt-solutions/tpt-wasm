# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities by opening a **private** GitHub Security Advisory
at https://github.com/tpt-solutions/tpt-wasm/security/advisories/new

Do not open a public issue for security vulnerabilities.

Include:
- A description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested mitigations

## Security model

TPT-Wasm treats each subsystem as a security boundary:

| Boundary | Threats |
|---|---|
| Decoder | Malformed binaries, oversized allocations, integer overflow, recursion/nesting exhaustion |
| Validator | Type confusion, index corruption |
| Interpreter | Memory OOB, table OOB, stack exhaustion, reference errors |
| Host | Capability escalation, confused deputy, resource lifetime violations, reentrancy, ambient authority |
| Compiler | Incorrect bounds elimination, control-flow corruption, register allocation bugs, executable-memory violations |

## Design invariants

- The Micro Interpreter targets zero `unsafe` blocks.
- The Host layer targets zero `unsafe` blocks.
- `unsafe` in the compiler is isolated to executable-memory and FFI boundaries; every block documents its safety invariant.
- Wasm traps are never represented as Rust panics.
- No Rust pointers or object addresses are exposed to Wasm programs.
- The host has no ambient authority; capabilities must be explicitly granted.
