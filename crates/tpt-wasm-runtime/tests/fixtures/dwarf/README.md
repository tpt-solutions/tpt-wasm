# DWARF fixtures

`add.c` compiled with clang 22 for `wasm32`, to check the source map against output from a real
toolchain rather than hand-built line tables. `tests/source_real.rs` reads them.

```sh
clang --target=wasm32 -O0 -g -gdwarf-4 -nostdlib -Wl,--no-entry \
  -Wl,--export=add -Wl,--export=twice -o add-v4.wasm add.c
clang --target=wasm32 -O0 -g -gdwarf-5 -nostdlib -Wl,--no-entry \
  -Wl,--export=add -Wl,--export=twice -o add-v5.wasm add.c
```

Line-table addresses are offsets from the start of the code section's contents, so a row for the
first instruction of a body is that body's offset in the section, not in the file.
