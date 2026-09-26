# Vendored spec test data

These files are copied unmodified from the WebAssembly specification repository:

    https://github.com/WebAssembly/spec/tree/main/test/core

| File | Purpose |
| --- | --- |
| `binary.wast` | The binary format: valid encodings, and `assert_malformed` cases for encodings the spec requires a decoder to reject. |
| `binary-leb128.wast` | LEB128 encodings, in particular the non-minimal ones. |
| `i32.wast` | Core suite: the full `i32` instruction set, execution semantics. |
| `i64.wast` | Core suite: the full `i64` instruction set, execution semantics. |
| `f32.wast` | Core suite: the full `f32` instruction set, including NaN bit-pattern rules. |
| `f64.wast` | Core suite: the full `f64` instruction set, including NaN bit-pattern rules. |
| `const.wast` | Core suite: constant encodings across all four numeric types. |
| `local_get.wast`, `local_set.wast`, `local_tee.wast` | Core suite: local variable access. |
| `nop.wast`, `unreachable.wast`, `return.wast` | Core suite: `nop`, `unreachable`, and early `return`. |
| `forward.wast`, `call.wast` | Core suite: forward references and direct calls. |
| `br_if.wast`, `labels.wast`, `switch.wast` | Core suite: conditional branch and label/branch-table semantics. |
| `stack.wast` | Core suite: operand stack behavior across calls. |
| `int_exprs.wast`, `int_literals.wast` | Core suite: integer edge-case expressions and literal encodings. |
| `float_exprs.wast`, `float_literals.wast`, `float_misc.wast` | Core suite: floating-point edge-case expressions, literal encodings, and miscellaneous float behavior. |
| `traps.wast` | Core suite: trap conditions not covered by the per-type instruction files. |
| `comments.wast` | Core suite: comment syntax in the text format. |

All Core suite files above were vendored from revision
`608711107b7f1edb13efd57b7d79b49477462d36` of the spec repository. `binary.wast`
and `binary-leb128.wast` predate this note; their revision was not recorded when
they were added.

They are vendored rather than fetched so that a test run needs no network and
always exercises exactly these bytes. Re-vendoring means copying the files again
and re-checking the assertion counts in `tests/binary_format.rs` and the
directive counts in `tests/core_spec.rs`, which are asserted so that a silent
change in coverage fails the build.

## License and provenance

The WebAssembly specification and its test suite are licensed under the
Apache License 2.0, Copyright The WebAssembly Authors. That is compatible with
this project's `MIT OR Apache-2.0`; the files keep their original content and are
not relicensed.

## How each suite is run

`binary_format.rs` reads only the `(module binary ...)` form: modules written in
the text format are reported as skipped rather than being run, because this
harness has no text assembler and reading one would be a second implementation
of the format whose bugs could mask a decoder bug.

`core_spec.rs` runs the Core suite's execution semantics (`module`, `invoke`,
`assert_return`, `assert_trap`, `assert_exhaustion`, `register`), which are
written almost entirely in the text format. Turning that text into bytes is
delegated to the `wast` crate (from `bytecodealliance/wasm-tools`, used as a
regular dependency of `tpt-wasm-spec`) for the text encoding step only; decode,
validate, instantiate, and call remain tpt-wasm's own code, so the thing
actually under test stays independent of the text encoder. `assert_invalid` and
`assert_malformed` directives in these files are reported as skipped, the same
reasoning as `binary_format.rs`: they are written in the text format, and this
harness does not re-derive a decode/validate outcome for text-format modules the
way it does for `(module binary ...)`.

The suite runs twice, once per execution backend, and both runs are held to the
same asserted directive counts:

- `core_spec_suite` decodes, validates, instantiates, and executes through
  `EngineMode::Micro` — the interpreter.
- `core_spec_suite_on_baseline` runs the identical directives through
  `EngineMode::Baseline`, so every module is lowered to the TPT IR, verified,
  code-generated into the portable baseline, and executed by it, and is held to
  upstream's own expected values, NaN patterns, and traps rather than to
  fixtures written beside the code under test.

The two share everything between the decoded bytes and the compared result, so
a directive that passes on one backend and fails on the other is a difference in
execution rather than in the harness. Running the suite on the baseline is what
surfaced the lowering defects recorded in `todo.md`; several of them — a loop's
result being dropped, an `else` arm being lowered into a terminated block, dead
code desynchronizing the body reader — were invisible to the hand-written
fixtures and to the seeded differential generator.
