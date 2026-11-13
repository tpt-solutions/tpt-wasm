//! Host calls made from native code, through `EngineMode::Jit`, checked against Micro and the baseline: the
//! results, the failures the host reports, and how many times the host ran.
#![cfg(feature = "jit")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tpt_wasm_runtime::{Config, Engine, EngineMode, HostFunction};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

fn section(bytes: &mut Vec<u8>, id: u8, content: &[u8]) {
    bytes.push(id);
    bytes.push(content.len() as u8);
    bytes.extend(content);
}

/// A module that imports `env.add: (i32) -> i32` and exports `f(x) = add(x) * 2`.
fn module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // type: (i32) -> i32
    section(&mut bytes, 1, &[0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]);
    // import: env.add, a function of type 0
    section(
        &mut bytes,
        2,
        &[
            0x01, 0x03, b'e', b'n', b'v', 0x03, b'a', b'd', b'd', 0x00, 0x00,
        ],
    );
    // one defined function, of type 0
    section(&mut bytes, 3, &[0x01, 0x00]);
    // export: f, the defined function (index 1, after the import)
    section(&mut bytes, 7, &[0x01, 0x01, b'f', 0x00, 0x01]);
    // f: local.get 0; call add; i32.const 2; i32.mul; end
    let body = [0x00, 0x20, 0x00, 0x10, 0x00, 0x41, 0x02, 0x6c, 0x0b];
    let mut code = vec![0x01, body.len() as u8];
    code.extend(body);
    section(&mut bytes, 10, &code);
    bytes
}

/// `add(x) = x + 1`, counting its calls.
struct AddOne {
    calls: Arc<AtomicUsize>,
}

impl HostFunction for AddOne {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match args {
            [Value::I32(value)] => Ok(vec![Value::I32(value.wrapping_add(1))]),
            _ => Err("add takes one i32".into()),
        }
    }
}

/// `add` that always fails, as a refused capability would.
struct RefusingAdd;

impl HostFunction for RefusingAdd {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
        Err("the host refused add".into())
    }
}

fn instance(
    mode: EngineMode,
    host: impl HostFunction + 'static,
) -> (Engine, tpt_wasm_runtime::Instance) {
    let mut engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    engine.linker_mut().define_function("env", "add", host);
    let instance = engine
        .instantiate_bytes(&module())
        .expect("module instantiates");
    (engine, instance)
}

/// The outcome of each call to `f`, in order.
fn outcomes(mode: EngineMode, host: impl HostFunction + 'static, arguments: &[i32]) -> Vec<String> {
    let (_engine, mut instance) = instance(mode, host);
    arguments
        .iter()
        .map(|argument| format!("{:?}", instance.call("f", vec![Value::I32(*argument)])))
        .collect()
}

#[test]
fn a_native_host_call_agrees_with_micro_and_the_baseline() {
    let calls = [0, 5, -3, i32::MAX];
    let golden = outcomes(
        EngineMode::Micro,
        AddOne {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        &calls,
    );
    for mode in [EngineMode::Baseline, EngineMode::Jit] {
        let counter = Arc::new(AtomicUsize::new(0));
        let got = outcomes(
            mode,
            AddOne {
                calls: counter.clone(),
            },
            &calls,
        );
        assert_eq!(got, golden, "{mode:?} agrees with Micro");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            calls.len(),
            "{mode:?} calls the host once per call to f"
        );
    }
    assert_eq!(golden[1], "Ok([I32(12)])", "f(5) = (5 + 1) * 2");
}

#[test]
fn a_native_function_that_makes_host_calls_runs_natively() {
    let (_engine, instance) = instance(
        EngineMode::Jit,
        AddOne {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    assert!(instance.runs_natively("f"), "f runs as machine code");
}

#[test]
fn a_host_failure_is_reported_the_same_way_on_every_engine() {
    let golden = outcomes(EngineMode::Micro, RefusingAdd, &[1]);
    for mode in [EngineMode::Baseline, EngineMode::Jit] {
        assert_eq!(
            outcomes(mode, RefusingAdd, &[1]),
            golden,
            "{mode:?} reports the host's failure as Micro does"
        );
    }
    assert!(
        golden[0].contains("the host refused add"),
        "the failure names the host's message: {}",
        golden[0]
    );
}

/// A module that imports `env.poke: (i32, i32) -> ()`, has one memory, and exports `g(addr)`, which pokes 42
/// at `addr` through the host and then loads the word at `addr` itself.
fn memory_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // types: (i32) -> i32; (i32, i32) -> ()
    section(
        &mut bytes,
        1,
        &[
            0x02, 0x60, 0x01, 0x7f, 0x01, 0x7f, 0x60, 0x02, 0x7f, 0x7f, 0x00,
        ],
    );
    // import: env.poke, a function of type 1
    section(
        &mut bytes,
        2,
        &[
            0x01, 0x03, b'e', b'n', b'v', 0x04, b'p', b'o', b'k', b'e', 0x00, 0x01,
        ],
    );
    // one defined function, of type 0
    section(&mut bytes, 3, &[0x01, 0x00]);
    // one memory, one page at least and no maximum
    section(&mut bytes, 5, &[0x01, 0x00, 0x01]);
    // exports: g (index 1, after the import), and the memory
    section(
        &mut bytes,
        7,
        &[
            0x02, 0x01, b'g', 0x00, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
        ],
    );
    // g: local.get 0; i32.const 42; call poke; local.get 0; i32.load; end
    let body = [
        0x00, 0x20, 0x00, 0x41, 0x2a, 0x10, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00, 0x0b,
    ];
    let mut code = vec![0x01, body.len() as u8];
    code.extend(body);
    section(&mut bytes, 10, &code);
    bytes
}

/// `poke(addr, value)`: writes `value` at `addr` through the memory the host is lent.
struct Poke;

impl HostFunction for Poke {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(vec![ValueType::I32, ValueType::I32]),
            results: ResultType(Vec::new()),
        }
    }

    fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
        Err("poke needs the module's memory".into())
    }

    fn call_with_memory(
        &self,
        memory: Option<&mut dyn tpt_wasm_types::GuestMemory>,
        args: &[Value],
    ) -> Result<Vec<Value>, String> {
        let (Some(memory), [Value::I32(address), Value::I32(value)]) = (memory, args) else {
            return Err("poke takes an address and a value, and the module's memory".into());
        };
        if memory.write(*address as u32 as u64, &value.to_le_bytes()) {
            Ok(Vec::new())
        } else {
            Err("poke is out of bounds".into())
        }
    }
}

#[test]
fn a_host_call_writes_memory_that_the_native_code_then_reads() {
    let run = |mode: EngineMode| -> (Vec<String>, bool) {
        let mut engine = Engine::new(Config {
            engine_mode: mode,
            ..Config::default()
        })
        .expect("engine");
        engine.linker_mut().define_function("env", "poke", Poke);
        let mut instance = engine
            .instantiate_bytes(&memory_module())
            .expect("module instantiates");
        let natively = instance.runs_natively("g");
        let results = [16, 1000]
            .iter()
            .map(|address| format!("{:?}", instance.call("g", vec![Value::I32(*address)])))
            .collect();
        (results, natively)
    };
    let (golden, _) = run(EngineMode::Micro);
    assert_eq!(golden, ["Ok([I32(42)])", "Ok([I32(42)])"]);
    for mode in [EngineMode::Baseline, EngineMode::Jit] {
        let (results, natively) = run(mode);
        assert_eq!(results, golden, "{mode:?} sees the host's write");
        if mode == EngineMode::Jit {
            assert!(natively, "g runs as machine code");
        }
    }
}
