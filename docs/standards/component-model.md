# Component Model

**Status:** Partly done (M10). `crates/tpt-wasm-component` decodes the envelope and the core-instance section, and instantiates core instances that have no arguments; imports between instances, inline exports, and the WIT mapping are not started.  
**Specification:** https://component-model.bytecodealliance.org/  

The Component Model is a higher-level layer over Core WebAssembly.

```
Component
   │
   ├── Core Wasm module(s)
   │
   ▼
Component runtime
   │
   ▼
TPT Host
```

The Micro Interpreter must not be polluted with Component Model concepts.
Core Wasm is the foundation; the Component Model builds on top.

## Integration path

```
Core Module → Component → WIT Interface → TPT Capability
```

The Component Model emphasizes capability-safe interfaces and fine-grained sandboxing,
making it a natural fit for the TPT Host architecture.

## Scope so far

`tpt-wasm-component` is a separate crate above Core Wasm, so the Micro interpreter and the
runtime do not depend on it; the component crate depends on the runtime to run the modules.
It decodes the component preamble (the magic, version `0x0d`, layer `0x01`), the section
framing, the core modules a component nests, and the core-instance section (id 2) as the
spec's grammar gives it: `0x00` instantiates a module, and `0x01` builds an instance from
inline exports. Each core instance is instantiated on an `Engine`, and its exports can be
called by name.

Decided on the recommendation: a section of any kind other than custom sections (skipped),
core modules (collected), and core instances is refused with its id and offset. Those
sections carry types, aliases, canonical functions, imports, and exports, and running a
component without them would be a silent partial run. Each is added with its own decoder
and tests.

Decided on the recommendation: an instance with arguments (imports from other instances)
and an inline-exports instance are refused. Both need the instance index space and the
core export index space, which come with the alias section. Until then a component can
run only modules that import nothing.

Not done: instance arguments and inline exports, the alias section, the component's type
and import checks, canonical lifting and lowering, and the WIT mapping to capabilities.

## Design constraint

The TPT Host ABI must not be designed in a way that makes future Component Model
integration impossible. This informs the capability and resource model design.
