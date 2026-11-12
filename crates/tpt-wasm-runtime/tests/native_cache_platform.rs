//! Where the native cache cannot check ownership and permissions, it does not open. Off Unix only.
#![cfg(all(feature = "jit", not(unix)))]

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
