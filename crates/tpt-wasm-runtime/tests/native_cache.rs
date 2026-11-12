//! The on-disk cache of compiled native code. Unix only, since the cache checks ownership and
//! permissions there.
#![cfg(all(feature = "jit", unix))]

use std::os::unix::fs::PermissionsExt;
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
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    let scratch = Scratch::new("reuse");
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
fn an_entry_that_another_user_could_write_is_refused_and_replaced() {
    let scratch = Scratch::new("permissions");
    let golden = fib_results(&Engine::new(Config::default()).expect("engine"));
    fib_results(&engine(scratch.path()).expect("the cache opens"));
    let entry = entries(scratch.path()).pop().expect("an entry was stored");
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o644))
        .expect("makes the entry readable by others");

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
    let mode = std::fs::metadata(&entry)
        .expect("the entry exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "the replacement is private to its owner");
}

#[test]
fn a_corrupt_entry_is_ignored_and_the_module_is_compiled() {
    let scratch = Scratch::new("corrupt");
    let golden = fib_results(&Engine::new(Config::default()).expect("engine"));
    fib_results(&engine(scratch.path()).expect("the cache opens"));
    let entry = entries(scratch.path()).pop().expect("an entry was stored");
    std::fs::write(&entry, b"not an artifact").expect("overwrites the entry");

    let next = engine(scratch.path()).expect("the cache opens");
    assert_eq!(fib_results(&next), golden);
    assert_eq!(next.native_cache_hits(), Some(0));
}

#[test]
fn a_directory_other_users_can_write_is_refused_at_open() {
    let scratch = Scratch::new("open-permissions");
    std::fs::create_dir_all(scratch.path()).expect("creates the directory");
    std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o777))
        .expect("makes the directory writable by others");
    assert!(
        engine(scratch.path()).is_err(),
        "a cache that others can write into is not opened"
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
