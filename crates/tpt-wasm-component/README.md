# tpt-wasm-component

The WebAssembly Component Model, as a layer above Core Wasm. The Micro interpreter knows
nothing about it, and this crate depends on the runtime to run the core modules a component
nests. The full scope, and what is still refused, is in
[`docs/standards/component-model.md`](../../docs/standards/component-model.md).

## What it does

- Decodes a component: the preamble, the sections, the nested core modules, and the type,
  core-instance, alias, import, canon, and export sections.
- Instantiates the core instances on an `Engine`, binding imports to earlier instances or to
  inline exports.
- Lifts exported functions over scalars (including `bool`, the small integers, and `char`),
  strings (UTF-8, UTF-16, and `latin1+utf16`), records, tuples, lists, options, enums, flags,
  results, and variants. Parameters past 16 flat values are passed indirectly.
- Lowers imported functions to core functions. Each import is granted through a
  `CapabilitySet`, checked at instantiation and on every call.
- Reads one or more WIT documents and maps a world's imports to capability ids, with
  `wit::capability_ids_across`.

## What it does not do yet

- Strings and lists in lowered imports (they need the caller's memory).
- Resources, `own` and `borrow`, futures and streams, and async functions.
- WIT beyond the packages given: fetching the packages a document names, and types and
  resources in the capability mapping.

## Example

```rust,ignore
use tpt_wasm_component::{Component, ComponentValue};
use tpt_wasm_runtime::{Config, Engine};

let component = Component::decode(&bytes)?;
let engine = Engine::new(Config::default())?;
let mut instance = component.instantiate(&engine)?;
let result = instance.call_export("greet", vec![ComponentValue::String("hi".into())])?;
```
