# Decoder fuzz target

This isolated cargo-fuzz project targets the WebAssembly binary decoder with
arbitrary byte slices. Run it on a Unix-like host with a nightly toolchain:

```text
cargo +nightly fuzz run decode
```

The target is intentionally outside the product workspace so normal builds do
not acquire a libFuzzer dependency. The decoder crate also contains a
 deterministic mutation test that runs as part of ordinary `cargo test`.
