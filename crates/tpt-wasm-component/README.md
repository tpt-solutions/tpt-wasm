# tpt-wasm-component

The outer layer of the WebAssembly Component Model. It decodes a component's envelope:
the preamble, the section framing, and the core modules a component nests. It sits above
Core Wasm, and the Micro interpreter knows nothing about it.

What it does not do yet: it refuses every section other than custom sections and core
modules, so a component that uses types, instances, canonical functions, imports, or
exports fails with `UnsupportedSection` rather than being half-run. Instantiating the
nested core modules, and the WIT mapping, come in later stages.
