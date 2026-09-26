# Vendored spec test data

These files are copied unmodified from the WebAssembly specification repository:

    https://github.com/WebAssembly/spec/tree/main/test/core

| File | Purpose |
| --- | --- |
| `binary.wast` | The binary format: valid encodings, and `assert_malformed` cases for encodings the spec requires a decoder to reject. |
| `binary-leb128.wast` | LEB128 encodings, in particular the non-minimal ones. |

They are vendored rather than fetched so that a test run needs no network and
always exercises exactly these bytes. Re-vendoring means copying the files again
and re-checking the assertion counts in `tests/binary_format.rs`, which are
asserted so that a silent change in coverage fails the build.

## License and provenance

The WebAssembly specification and its test suite are licensed under the
Apache License 2.0, Copyright The WebAssembly Authors. That is compatible with
this project's `MIT OR Apache-2.0`; the files keep their original content and are
not relicensed.

Only the `(module binary ...)` form is read. Modules written in the text format
are reported as skipped rather than being run, because this harness has no text
assembler and reading one would be a second implementation of the format whose
bugs could mask a decoder bug.
