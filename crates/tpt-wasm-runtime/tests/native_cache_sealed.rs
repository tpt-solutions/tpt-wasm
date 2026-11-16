// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The native cache, sealed under an embedder's key. Runs on Unix and Windows, where the cache is
//! supported, so it is not limited to one platform the way the permission tests are.
#![cfg(all(feature = "jit", any(unix, windows)))]

use std::path::{Path, PathBuf};

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// `fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2)`, the module the native tier compiles with its calls.
const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f, // type section: (i32) -> i32
    0x03, 0x02, 0x01, 0x00, // function section
    0x07, 0x07, 0x01, 0x03, 0x66, 0x69, 0x62, 0x00, 0x00, // export "fib"
    0x0a, 0x1e, 0x01, 0x1c, 0x00, // code section
    0x20, 0x00, 0x41, 0x02, 0x48, 0x04, 0x7f, 0x20, 0x00, 0x05, 0x20, 0x00, 0x41, 0x01, 0x6b, 0x10,
    0x00, 0x20, 0x00, 0x41, 0x02, 0x6b, 0x10, 0x00, 0x6a, 0x0b, 0x0b,
];

const KEY: &[u8] = b"an embedder key that is long enough to seal";
const OTHER_KEY: &[u8] = b"a different key that is also long enough!";

/// A fresh directory for one test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("tpt-sealed-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn engine(dir: &Path, key: Option<&[u8]>) -> Result<Engine, tpt_wasm_runtime::RuntimeError> {
    Engine::new(Config {
        engine_mode: EngineMode::Jit,
        native_cache_dir: Some(dir.to_path_buf()),
        native_cache_key: key.map(<[u8]>::to_vec),
        ..Config::default()
    })
}

/// Compile `fib` through `engine`, which stores or loads its entry, and return what it computes.
fn compile(engine: &Engine) -> String {
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    format!("{:?}", instance.call("fib", vec![Value::I32(10)]))
}

/// The cache entries in `dir`, skipping the temporaries a store writes before it renames.
fn entries(dir: &Path) -> Vec<Vec<u8>> {
    std::fs::read_dir(dir)
        .expect("the cache directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tptaot"))
        .map(|path| std::fs::read(path).expect("an entry reads"))
        .collect()
}

#[test]
fn an_entry_written_under_a_key_is_sealed_and_loads_under_that_key() {
    let scratch = Scratch::new("sealed");
    let first = engine(scratch.path(), Some(KEY)).expect("an engine with a key");
    let answer = compile(&first);
    let stored = entries(scratch.path());
    assert!(!stored.is_empty(), "the compilation was stored");
    for bytes in &stored {
        assert!(
            tpt_wasm_aot::ModuleArtifact::from_bytes(bytes).is_err(),
            "a stored entry is sealed, not a plain artifact"
        );
        assert!(
            tpt_wasm_aot::seal::open(bytes, KEY).is_ok(),
            "the entry opens under the key that wrote it"
        );
    }
    let second = engine(scratch.path(), Some(KEY)).expect("a second engine");
    assert_eq!(compile(&second), answer);
    assert_eq!(
        second.native_cache_hits(),
        Some(1),
        "the second engine loaded it"
    );
}

#[test]
fn an_entry_under_another_key_is_not_loaded() {
    let scratch = Scratch::new("other-key");
    let first = engine(scratch.path(), Some(KEY)).expect("an engine with a key");
    compile(&first);
    let other = engine(scratch.path(), Some(OTHER_KEY)).expect("an engine with another key");
    assert_eq!(compile(&other), compile(&first));
    assert_eq!(
        other.native_cache_hits(),
        Some(0),
        "the entry did not open, so it was compiled"
    );
}

#[test]
fn an_unsealed_entry_is_not_loaded_by_an_engine_with_a_key() {
    let scratch = Scratch::new("unsealed");
    let plain = engine(scratch.path(), None).expect("an engine without a key");
    compile(&plain);
    let sealed = engine(scratch.path(), Some(KEY)).expect("an engine with a key");
    compile(&sealed);
    assert_eq!(
        sealed.native_cache_hits(),
        Some(0),
        "a plain entry is a miss for a sealed cache, so it is recompiled"
    );
}

#[test]
fn a_key_shorter_than_the_minimum_is_refused_at_engine_creation() {
    let scratch = Scratch::new("short-key");
    assert!(engine(scratch.path(), Some(b"too short")).is_err());
}
