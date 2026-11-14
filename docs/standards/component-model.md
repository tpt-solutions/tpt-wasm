# Component Model

**Status:** Partly done (M10). `crates/tpt-wasm-component` decodes the envelope, the type, canon, alias, core-instance, and export sections; instantiates core instances with imports bound to earlier instances or to inline exports; lifts exported functions over scalar types and UTF-8 strings; and takes imported functions from the embedder, lowering them so core code can call them. The WIT mapping and the remaining value types are not started.  
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

An instance argument binds one import module name to an earlier core instance, so every
import under that name resolves to that instance's exports. The arguments name only
instances already declared, which is checked at decode time, and a name may be bound once.
Each core instance gets a linker of its own, through `Engine::instantiate_bytes_with_linker`,
so one instance's imports cannot leak into another.

The alias section (id 6) defines core functions: an alias takes an export of an earlier
core instance that is a module instance. Other alias forms (other core sorts, outer aliases,
aliases of component exports, and aliases of inline-exports instances) are refused.

An inline-exports instance (`0x01`) groups core functions under names, and creates no
runtime instance of its own. An import bound to it resolves each name to the function it
names, through `Linker::define_export`, and a call into it runs that function in the
instance that defines it. Each name is checked at instantiation, so a missing export is an
error then, not at the first call.

Canonical lifting, on the recommendation, covers the scalar subset only: `s32`, `u32`,
`s64`, `u64`, `f32`, and `f64`. Each flattens to one core value of the same width, and the
conversion is a reinterpretation of the bits, so no value changes. A canon lift (`0x00 0x00`)
names a core function and a component function type, and takes no canonical options. A
component function's flattened type (one core parameter per scalar, and at most one core
result) must equal the core function's type, which is read from the nested module's bytes
at instantiation, so a mismatch fails there and not at the first call. An export names a
component function (`0x01`) with no extern type; each export is then callable with
`ComponentInstance::call_export`, which checks the argument count and types and lifts the
result.

Imports (section 10, `0x00` name and `0x01` function type) are functions the embedder supplies
by name through `Imports`. A component that imports a name the embedder did not supply
fails with `MissingImport` before anything runs. Canon lower (`0x01 0x00`) turns an import into
a core function. That core function reaches a core module through an inline-exports instance,
and binding it as an import registers the embedder's function as a host function, so the
core module calls the embedder's closure with lifted arguments and lowers the results. The
closure's type is not checked against the import's declared type, since a Rust closure has
no type to check; the call still checks the number and kinds of its arguments and results.
Lowering a function the component defines, and lifting a lowered function, are refused,
because they would need a component to call back into itself.

Refused, each by name and offset, until its own decoder exists: canonical options (strings,
memory, and realloc, which need the lifting of linear memory), the other value types
(`bool`, the small integers, `char`, `string`, records, lists, and the rest), extern types on
exports, and exports of any sort other than function. Exports of imported functions are
refused too.

The decoder and the instantiation path take untrusted bytes, so a test truncates every
fixture at every length and corrupts each byte in four ways. Each variant that decodes is
instantiated and called, and none may panic.

Strings are supported for lifted exports, as UTF-8 through the canonical `memory` and
`realloc` options. A string flattens to two core values, a pointer and a length, and a string
result is returned as a pointer to that pair in memory. A string argument is copied in with
`realloc(0, 0, 1, length)`, the canonical UTF-8 copy, and then written into the memory with
`Instance::write_memory`. A string result is read back from the pointer the function returns,
and must be valid UTF-8. The flattening rules (a maximum of 16 flat parameters, one flat result,
and the return-pointer convention for a multi-value result) are the canonical ABI's, and the
crate refuses a type that would need the indirect form rather than implementing it.

The rules were checked against a mirror of the Canonical ABI text, `clunt.github.io/component-model/design/mvp/CanonicalABI.html`,
because the canonical file itself did not render for the fetch. They should be checked again
against the canonical file before a release.

Refused, each by name and offset: a string in a lowered import (the host call would need the
caller's memory), more than 16 flat parameters, a memory or `realloc` from another instance,
any other canonical option, and a string without the options it needs.

Not done: strings in lowered imports, UTF-16 and Latin-1 encodings, `post-return`, the other
value types (`bool`, the small integers, `char`, records, lists), the component's import type
checks against the embedder's functions, and the WIT mapping to capabilities.

## Design constraint

The TPT Host ABI must not be designed in a way that makes future Component Model
integration impossible. This informs the capability and resource model design.
