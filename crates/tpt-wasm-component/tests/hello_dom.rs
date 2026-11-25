//! A real componentized guest, built from tpt-helix's `helix-guest` world (`tests/fixtures/helix.wit`)
//! by `wit-component`, runs through the component layer. It imports the `dom`, `storage` and
//! `network` interfaces as instances, wires them through two shim core modules and a table, and
//! exports `run`. The host here records each call.

use std::sync::{Arc, Mutex};

use tpt_wasm_capability::{
    CapabilityDescriptor, CapabilityGrant, CapabilityId, CapabilitySet, Operation, Permissions,
};
use tpt_wasm_component::wit::WitDocument;
use tpt_wasm_component::{Component, ComponentValue, Imports};

const GUEST: &[u8] = include_bytes!("fixtures/hello_dom.wasm");
const WORLD: &str = include_str!("fixtures/helix.wit");

/// The interface functions the guest imports, by interface and name.
const FUNCTIONS: [(&str, &str); 8] = [
    ("dom", "create-element"),
    ("dom", "set-text"),
    ("dom", "set-attribute"),
    ("dom", "append-child"),
    ("dom", "on-click"),
    ("dom", "on-event"),
    ("storage", "set"),
    ("network", "fetch"),
];

/// Imports for the guest: each function is typed from `helix.wit`, granted `call`, and records its
/// arguments. The log is returned so a test can read the calls the guest made.
fn host() -> (Imports, Arc<Mutex<Vec<String>>>) {
    let document = WitDocument::parse(WORLD).expect("helix.wit parses");
    let log = Arc::new(Mutex::new(Vec::new()));
    let next_id = Arc::new(Mutex::new(0u64));
    let mut set = CapabilitySet::default();
    let mut typed = Vec::new();
    for (interface, function) in FUNCTIONS {
        let (table, ty) = document
            .function_signature(interface, function)
            .expect("the signature is laid out");
        let name = format!("helix:runtime/{interface}@0.1.0#{function}");
        set.register(CapabilityDescriptor {
            id: CapabilityId::new(&name),
            operations: vec![Operation::execute("call")],
        })
        .expect("registers");
        set.grant(CapabilityGrant {
            capability: CapabilityId::new(&name),
            permissions: Permissions {
                execute: true,
                ..Permissions::default()
            },
        })
        .expect("grants");
        typed.push((name, table, ty));
    }
    let mut imports = Imports::with_capabilities(Arc::new(Mutex::new(set)));
    for (name, table, ty) in typed {
        let label = name
            .trim_start_matches("helix:runtime/")
            .replace("@0.1.0#", ".");
        let log = log.clone();
        let next_id = next_id.clone();
        imports.define_typed_function(name, table, ty, move |args: &[ComponentValue]| {
            log.lock().unwrap().push(format!("{label} {args:?}"));
            Ok(match label.as_str() {
                "dom.create-element" => {
                    let mut next = next_id.lock().unwrap();
                    *next += 1;
                    vec![ComponentValue::Record(vec![(
                        "id".to_owned(),
                        ComponentValue::U64(*next),
                    )])]
                }
                "storage.set" => vec![ComponentValue::Result(Ok(None))],
                // The guest's request has no route, so the host answers with an error.
                "network.fetch" => vec![ComponentValue::Result(Err(Some(Box::new(
                    ComponentValue::String("no route".to_owned()),
                ))))],
                _ => Vec::new(),
            })
        });
    }
    (imports, log)
}

#[test]
fn the_guest_runs_and_makes_its_calls_in_order() {
    let component = Component::decode(GUEST).expect("the guest decodes");
    let (imports, log) = host();
    let engine =
        tpt_wasm_runtime::Engine::new(tpt_wasm_runtime::Config::default()).expect("engine");
    let mut instance = component
        .instantiate_with(&engine, &imports)
        .expect("the guest instantiates");
    assert_eq!(instance.call_export("run", Vec::new()), Ok(Vec::new()));

    let calls = log.lock().unwrap();
    assert_eq!(calls.len(), 9, "the guest makes nine host calls: {calls:?}");
    assert_eq!(calls[0], r#"dom.create-element [String("div")]"#);
    assert_eq!(calls[1], r#"dom.create-element [String("span")]"#);
    assert!(calls[2].starts_with("dom.set-text [Record([(\"id\", U64(2))])"));
    assert!(calls[3].contains(r#"String("class"), String("greeting")"#));
    assert!(calls[4].starts_with("dom.append-child"));
    assert!(calls[5].starts_with("dom.on-click") && calls[5].ends_with("U64(1)]"));
    assert!(calls[6].starts_with("dom.on-event") && calls[6].contains(r#"String("submit")"#));
    // The stored value is the bytes of "yes".
    assert_eq!(
        calls[7],
        "storage.set [String(\"seen\"), List([U8(121), U8(101), U8(115)])]"
    );
    assert!(calls[8].starts_with("network.fetch") && calls[8].contains(r#"String("GET")"#));
}

#[test]
fn a_guest_with_no_grants_is_refused_before_it_runs() {
    let component = Component::decode(GUEST).expect("the guest decodes");
    let engine =
        tpt_wasm_runtime::Engine::new(tpt_wasm_runtime::Config::default()).expect("engine");
    let result = component.instantiate_with(&engine, &Imports::new());
    assert!(
        result.is_err(),
        "an unsupplied import is refused at instantiation"
    );
}
