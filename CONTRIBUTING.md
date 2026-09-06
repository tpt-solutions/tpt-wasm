# Contributing to TPT-Wasm

## Development model

Contributions and project discussion are currently managed through GitHub Issues.

Issues may cover:

- bugs
- feature requests
- standards compatibility
- security issues
- architecture decisions
- research questions
- interoperability

The project is currently maintained by the project owner.
**Pull requests are not part of the normal development workflow.**

## Independent implementation policy

TPT-Wasm is implemented independently from existing WebAssembly runtimes.

The project may use:
- public WebAssembly specifications (W3C, WASI, Component Model)
- standards documents and proposals
- published academic work
- public test suites (for interoperability testing only)
- behavioral interoperability testing against other runtimes
- publicly documented algorithms

**Implementation code from external runtimes must not be incorporated**
unless its license and provenance have been explicitly reviewed and the use
is compatible with the project's licensing policy (`MIT OR Apache-2.0`).

This is an engineering and provenance policy. It ensures that every significant
piece of code in this repository can be attributed to its origin.

## Standards

The normative foundation for all implementation decisions is:

- [WebAssembly Core Specification](https://webassembly.github.io/spec/)

Not another runtime's behavior, not informal documentation.

## Security

Please see [SECURITY.md](SECURITY.md) for the security contact and disclosure process.
