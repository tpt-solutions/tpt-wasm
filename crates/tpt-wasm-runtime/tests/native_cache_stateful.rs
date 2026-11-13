//! A module with a global is stored in the native cache like any other, and a later engine loads it with
//! its global starting fresh, as a compiled one does. Unix and Windows, where the cache opens.
#![cfg(all(feature = "jit", any(unix, windows)))]

use std::path::{Path, PathBuf};

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

fn section(bytes: &mut Vec<u8>, id: u8, content: &[u8]) {
    bytes.push(id);
    bytes.push(content.len() as u8);
    bytes.extend(content);
}

/// A module with a mutable `i32` global `counter` starting at zero, and `bump(x)`, which adds `x` to it.
fn module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    section(&mut bytes, 1, &[0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]);
    section(&mut bytes, 3, &[0x01, 0x00]);
    section(&mut bytes, 6, &[0x01, 0x7f, 0x01, 0x41, 0x00, 0x0b]);
    let mut exports = vec![0x02, 0x04];
    exports.extend(b"bump");
    exports.extend([0x00, 0x00, 0x07]);
    exports.extend(b"counter");
    exports.extend([0x03, 0x00]);
    section(&mut bytes, 7, &exports);
    let body = [
        0x00, // no locals
        0x23, 0x00, 0x20, 0x00, 0x6a, 0x24, 0x00, 0x23, 0x00, 0x0b,
    ];
    let mut code = vec![0x01, body.len() as u8];
    code.extend(body);
    section(&mut bytes, 10, &code);
    bytes
}

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

fn engine(dir: &Path) -> Engine {
    Engine::new(Config {
        engine_mode: EngineMode::Jit,
        native_cache_dir: Some(dir.to_path_buf()),
        ..Config::default()
    })
    .expect("engine")
}

#[test]
fn a_module_with_a_global_is_stored_and_loaded_with_its_global_fresh() {
    let scratch = Scratch::new("stateful");
    let first = engine(scratch.path());
    let mut instance = first.instantiate_bytes(&module()).expect("instantiates");
    assert_eq!(first.native_cache_hits(), Some(0));
    assert!(instance.runs_natively("bump"));
    assert!(instance.call("bump", vec![Value::I32(5)]).is_ok());
    assert!(instance.call("bump", vec![Value::I32(2)]).is_ok());
    let stored = std::fs::read_dir(scratch.path())
        .expect("reads the cache")
        .count();
    assert_eq!(stored, 1, "the module with a global is stored");

    let second = engine(scratch.path());
    let mut loaded = second.instantiate_bytes(&module()).expect("instantiates");
    assert_eq!(
        second.native_cache_hits(),
        Some(1),
        "the stored module is loaded"
    );
    assert!(loaded.runs_natively("bump"));
    assert_eq!(
        format!("{:?}", loaded.call("bump", vec![Value::I32(7)])),
        "Ok([I32(7)])",
        "the loaded module starts its global at zero"
    );
    assert_eq!(format!("{:?}", loaded.global("counter")), "Ok(I32(7))");
}
