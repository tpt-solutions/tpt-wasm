# tpt-wasm-jit

The JIT compiler for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

> **Status: partial.** The crate compiles a subset of the IR to x86-64, AArch64, and RISC-V 64
> machine code, and that subset is checked against the Micro interpreter. The `Engine` reaches it
> through `EngineMode::Jit` in `tpt-wasm-runtime`, which needs that crate's `jit` feature: each
> function the compiler accepts runs natively, and the rest run on the baseline.

## What it compiles

A function whose parameters and result are `i32`, `i64`, `f32`, or `f64`, and whose body is:

- `i32`, `i64`, `f32`, and `f64` constants; reads and writes of parameters and declared locals
  (`local.get`, `local.set`, `local.tee`);
- the integer operations `add`, `sub`, `mul`, `and`, `or`, `xor`, the shifts `shl`, `shr_s`,
  and `shr_u`, the comparisons, `eqz`, and `select`, at both widths;
- the float operations `add`, `sub`, `mul`, `div`, `min`, `max`, `copysign`, `sqrt`, the rounding
  functions `ceil`, `floor`, `trunc`, and `nearest`, the comparisons (with NaN handled as Wasm
  requires), and `neg` and `abs`. On x86-64 the rounding functions need SSE4.1, so the target name
  records it, and they are refused on a host without it;
- integer loads and stores of every width, and `f32` and `f64` loads and stores;
- direct calls to other functions of the same module, when they are compiled together with
  `compile_module`; a call is made through a table of entry points in the call context, and
  a call past the call-depth limit traps as it does on Micro;
- indirect calls through the module's table, when that table is defined in the module and not
  exported (so no other instance can write it). The index and the slot's signature are checked
  in the order Micro checks them, and each slot must hold a function compiled in the same module;
- unconditional and conditional branches, including loops, and a return.

Everything else is refused with `JitError::Unsupported`. That includes the conversions between
integers and floats, calls through an imported or exported table, globals, memory growth, and
frames larger than one page. A function that calls one of those is refused too. A single function compiled with
`compile` on its own cannot call anything, since a call needs the module it belongs to.

Division and remainder, signed and unsigned, are compiled with Wasm's traps. A zero divisor
and `i32::MIN / -1` return `JitError::Trap`, the same trap Micro reports. The return value
is 64 bits: the low half is the result and the high half carries a trap status, so a trap
needs no separate channel.

## Use

```rust
let function: &tpt_wasm_ir::IrFunction = /* a verified, lowered function */;
let compiled = tpt_wasm_jit::compile(function)?;
let result = compiled.call(&[7, 35])?;
```

A compiled function takes any number of `i32` arguments. Memory is passed as a slice, and a call
without memory traps on any access.

```rust
let mut memory = vec![0u8; 65536];
let result = compiled.call_with_memory(&[16, 7], &mut memory)?;
```

## Safety boundary

The crate denies `unsafe` code. The one module that uses it, `exec`, maps executable memory:
it writes the code to a read-write page, then changes the page to read-execute, and never has
a page that is both writable and executable. Each `unsafe` block states the invariant it relies
on. The encoder is safe code and is tested on every target.

See [`docs/security/threat-model.md`](../../docs/security/threat-model.md) for what this
boundary does and does not guard against.

## Testing

- The encoder's golden encodings and register tests run everywhere.
- `tests/differential.rs` generates straight-line functions, runs each through Micro and
  through the compiled code, and requires equal results.
- `tests/control_flow.rs` does the same for loops, conditional branches, `select`,
  `local.tee`, shifts past the operand width, and comparisons. A guard requires the results
  to vary, so agreement cannot pass vacuously.
- `tests/memory.rs` compares loads, stores, traps, and the full memory contents with Micro over
  random widths, signedness and offsets, with addresses near the page end.
- `tests/division.rs` compares results and traps for all four division operations over
  operands that include zero, `-1`, and the ends of the `i32` range. Each outcome must occur.
- Both test files run on x86-64 Windows and, under WSL, on x86-64 Linux, so both calling
  conventions are exercised. Under QEMU user mode the same tests run the AArch64 and RISC-V
  code generators against Micro.

## Targets

| Target | Calling convention | Code generator | Tested |
|---|---|---|---|
| x86-64 Windows | Win64 | `x86.rs` | natively |
| x86-64 Unix | System V | `x86.rs` | natively under WSL |
| AArch64 Unix | AAPCS64 | `aarch64.rs` | under `qemu-aarch64` |
| RISC-V 64 Unix | LP64 | `riscv64.rs` | under `qemu-riscv64` |

Every encoder is compiled on every host, so its golden tests run everywhere. Installing code
needs the target.
- On other targets the compiler refuses with `JitError::UnsupportedTarget`, after the encoder
  has accepted or refused the function.
