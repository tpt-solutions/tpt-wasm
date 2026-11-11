# tpt-wasm-decode

The WebAssembly binary format decoder and encoder for
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

`decode` turns bytes into a structural `Module`. `encode` turns a `Module` back into bytes.

Decoding is kept strictly separate from validation and execution. A module that decodes
successfully may still fail validation. A module that fails to decode never reaches the
validator.

> Pre-1.0. Most users should use the [`tpt-wasm`](https://docs.rs/tpt-wasm) facade, which
> re-exports `decode` and `encode`.

## Usage

```rust
use tpt_wasm_decode::{decode, encode};

let bytes = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
let module = decode(&bytes).expect("a valid, empty module");
let round_trip = encode(&module).expect("encodes back to bytes");
assert_eq!(round_trip, bytes);
```

## What it covers

- Every section of the MVP binary format, including custom sections.
- All eight element-segment encodings.
- Data segments in each of their modes (`DataMode`).
- Errors are reported as `DecodeError` (for bytes) and `EncodeError` (for modules), not as
  panics.

## Testing

The crate is checked against the upstream WebAssembly binary-format test suite through the
harness in `tpt-wasm-spec`. Malformed-module cases must fail to decode. Invalid-module cases
must decode and then fail validation.

```sh
cargo test -p tpt-wasm-decode
cargo test -p tpt-wasm-spec
```

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-format`

## Safety

No `unsafe` code.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
