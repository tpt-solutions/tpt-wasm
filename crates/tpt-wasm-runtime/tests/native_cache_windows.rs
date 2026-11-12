//! The on-disk cache of compiled native code on Windows, where ownership and access are checked
//! through access control lists. The grants are made with `icacls`, naming the Everyone group by its
//! well-known SID so the test does not depend on the locale.
#![cfg(all(feature = "jit", windows))]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// The Everyone group, by its well-known SID.
const EVERYONE: &str = "*S-1-1-0";

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

/// A fresh directory for one test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("tpt-native-cache-{name}-{}", std::process::id()));
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

/// Add an access control entry to `path` for `grant`, such as `*S-1-1-0:W`.
fn grant(path: &Path, grant: &str) {
    let status = Command::new("icacls")
        .arg(path)
        .arg("/grant")
        .arg(grant)
        .stdout(Stdio::null())
        .status()
        .expect("runs icacls");
    assert!(status.success(), "icacls grants {grant} on {path:?}");
}

fn engine(dir: &Path) -> Result<Engine, tpt_wasm_runtime::RuntimeError> {
    Engine::new(Config {
        engine_mode: EngineMode::Jit,
        native_cache_dir: Some(dir.to_path_buf()),
        ..Config::default()
    })
}

/// The results of `fib(n)` for each `n`, on an instance from `engine`.
fn fib_results(engine: &Engine) -> Vec<String> {
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    (0..15)
        .map(|n| format!("{:?}", instance.call("fib", vec![Value::I32(n)])))
        .collect()
}

/// The entry files in the cache directory.
fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("reads the cache directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tptaot"))
        .collect()
}

#[test]
fn a_compiled_module_is_loaded_from_the_cache_by_the_next_engine() {
    let scratch = Scratch::new("win-reuse");
    let golden = fib_results(&Engine::new(Config::default()).expect("engine"));

    let first = engine(scratch.path()).expect("the cache opens");
    assert_eq!(first.native_cache_hits(), Some(0));
    assert_eq!(
        fib_results(&first),
        golden,
        "the first engine agrees with Micro"
    );
    assert_eq!(
        entries(scratch.path()).len(),
        1,
        "the compiled module is stored"
    );

    let second = engine(scratch.path()).expect("the cache opens");
    assert_eq!(
        fib_results(&second),
        golden,
        "a cached module agrees with Micro"
    );
    assert_eq!(
        second.native_cache_hits(),
        Some(1),
        "the second engine loads it"
    );
}

#[test]
fn an_entry_another_account_can_write_is_refused_and_replaced() {
    let scratch = Scratch::new("win-permissions");
    let golden = fib_results(&Engine::new(Config::default()).expect("engine"));
    fib_results(&engine(scratch.path()).expect("the cache opens"));
    let entry = entries(scratch.path()).pop().expect("an entry was stored");
    grant(&entry, &format!("{EVERYONE}:W"));

    let next = engine(scratch.path()).expect("the cache opens");
    assert_eq!(
        fib_results(&next),
        golden,
        "a refused entry is compiled again, correctly"
    );
    assert_eq!(
        next.native_cache_hits(),
        Some(0),
        "the refused entry was not used"
    );

    let after = engine(scratch.path()).expect("the cache opens");
    fib_results(&after);
    assert_eq!(
        after.native_cache_hits(),
        Some(1),
        "the replacement is trusted and loads from the cache"
    );
}

#[test]
fn a_read_only_grant_to_another_account_does_not_refuse_an_entry() {
    let scratch = Scratch::new("win-read-only");
    let golden = fib_results(&Engine::new(Config::default()).expect("engine"));
    fib_results(&engine(scratch.path()).expect("the cache opens"));
    let entry = entries(scratch.path()).pop().expect("an entry was stored");
    grant(&entry, &format!("{EVERYONE}:R"));

    let next = engine(scratch.path()).expect("the cache opens");
    assert_eq!(fib_results(&next), golden);
    assert_eq!(
        next.native_cache_hits(),
        Some(1),
        "reading does not let anyone inject code, so the entry is still used"
    );
}

#[test]
fn a_corrupt_entry_is_ignored_and_the_module_is_compiled() {
    let scratch = Scratch::new("win-corrupt");
    let golden = fib_results(&Engine::new(Config::default()).expect("engine"));
    fib_results(&engine(scratch.path()).expect("the cache opens"));
    let entry = entries(scratch.path()).pop().expect("an entry was stored");
    std::fs::write(&entry, b"not an artifact").expect("overwrites the entry");

    let next = engine(scratch.path()).expect("the cache opens");
    assert_eq!(fib_results(&next), golden);
    assert_eq!(next.native_cache_hits(), Some(0));
}

#[test]
fn a_directory_another_account_can_write_is_refused_at_open() {
    let scratch = Scratch::new("win-open-permissions");
    std::fs::create_dir_all(scratch.path()).expect("creates the directory");
    grant(scratch.path(), &format!("{EVERYONE}:(OI)(CI)W"));
    assert!(
        engine(scratch.path()).is_err(),
        "a cache that other accounts can write into is not opened"
    );
}

#[test]
fn an_engine_without_a_cache_reports_no_hits() {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Jit,
        ..Config::default()
    })
    .expect("engine");
    assert_eq!(engine.native_cache_hits(), None);
}
