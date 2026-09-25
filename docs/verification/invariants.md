# Executable Invariants

The current V0 implementation checks structural invariants of the Rust abstract
machine in `tpt-wasm-semantics`. The checks are deliberately conservative and
are run by `Configuration::check_invariants` and
`tpt_wasm_verify::check_v0`.

## Checked properties

1. Every active frame references a function in the store.
2. Every active frame program counter is below the function body length.
3. Frame locals and return arity agree with the function signature.
4. Operand and control bases are ordered across frames and bounded by their
   corresponding stacks.
5. Control-frame stack heights do not exceed the operand stack.
6. Each global value has its declared value type.

## Scope

These checks describe only the currently modeled instruction subset. They are
not a proof of WebAssembly validation, full Core execution, memory safety, or
host capability safety. The corresponding formal targets and remaining Lean
work are tracked in `docs/verification/model.md` and `todo.md`.
