//! Where the native cache cannot check ownership and access, it does not open. Only on platforms that
//! are neither Unix nor Windows.
#![cfg(all(feature = "jit", not(any(unix, windows))))]

use tpt_wasm_runtime::{Config, Engine, EngineMode};

#[test]
fn the_native_cache_is_refused_where_ownership_cannot_be_checked() {
    let dir = std::env::temp_dir().join("tpt-native-cache-platform");
    let result = Engine::new(Config {
        engine_mode: EngineMode::Jit,
        native_cache_dir: Some(dir),
        ..Config::default()
    });
    assert!(
        result.is_err(),
        "the cache needs owner and permission checks, which this platform does not offer here"
    );
}
