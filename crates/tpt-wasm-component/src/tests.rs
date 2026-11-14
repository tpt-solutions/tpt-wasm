use super::*;

/// A core module with the magic and version 1 and no sections.
const EMPTY_MODULE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/// A core module that exports `answer`, a function of no parameters returning `i32` 42.
const ANSWER_MODULE: [u8; 39] = [
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic and version
    0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
    0x03, 0x02, 0x01, 0x00, // function 0 has type 0
    0x07, 0x0a, 0x01, 0x06, 0x61, 0x6e, 0x73, 0x77, 0x65, 0x72, 0x00, 0x00, // export "answer"
    0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b, // code: i32.const 42
];

/// A core module that imports `env.answer`, a function of no parameters returning `i32`,
/// and exports `relay`, which returns what `answer` returns.
const RELAY_MODULE: [u8; 54] = [
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic and version
    0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
    0x02, 0x0e, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x06, 0x61, 0x6e, 0x73, 0x77, 0x65, 0x72, 0x00,
    0x00, // import env.answer, of type 0
    0x03, 0x02, 0x01, 0x00, // function 1 has type 0
    0x07, 0x09, 0x01, 0x05, 0x72, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x01, // export "relay"
    0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b, // code: call 0, end
];

/// A component type `() -> s32`.
const TYPE_NO_ARGUMENTS_S32: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, VALTYPE_S32];
/// A component type `() -> u32`.
const TYPE_NO_ARGUMENTS_U32: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, VALTYPE_U32];
/// A component type `() -> s64`, which does not match the i32 the module returns.
const TYPE_NO_ARGUMENTS_S64: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, VALTYPE_S64];
/// A component type `() -> ()`, which does not match the module's result.
const TYPE_NO_RESULT: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 1, 0];
/// The canon that lifts core function 0 with no options, using type 0.
const CANON_LIFT_ANSWER: [u8; 8] = [SECTION_CANON, 6, 1, CANON_LIFT, SORT_FUNC, 0, 0, 0];
/// The export of component function 0 as `answer`.
const EXPORT_ANSWER: [u8; 14] = [
    SECTION_EXPORT,
    12,
    1,
    0x00,
    6,
    b'a',
    b'n',
    b's',
    b'w',
    b'e',
    b'r',
    COMPONENT_SORT_FUNC,
    0,
    0x00,
];

fn with(sections: &[u8]) -> Vec<u8> {
    let mut bytes = PREAMBLE.to_vec();
    bytes.extend_from_slice(sections);
    bytes
}

/// A section with `content`. Its size is an unsigned LEB128, as the format gives it, so a section
/// of 127 bytes or fewer is one size byte.
fn section(id: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    let mut size = content.len();
    loop {
        let byte = (size & 0x7f) as u8;
        size >>= 7;
        if size == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend_from_slice(content);
    out
}

fn module_section(module: &[u8]) -> Vec<u8> {
    section(SECTION_CORE_MODULE, module)
}

/// Core instance 0 instantiates module 0 with no arguments.
fn instantiate_module_zero() -> Vec<u8> {
    vec![SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]
}

/// Core function 0 is the export `answer` of core instance 0.
fn alias_answer() -> Vec<u8> {
    vec![
        SECTION_ALIAS,
        12,
        1,
        SORT_CORE_WRAPPER,
        SORT_FUNC,
        ALIAS_CORE_EXPORT,
        0,
        6,
        b'a',
        b'n',
        b's',
        b'w',
        b'e',
        b'r',
    ]
}

/// Core instance 1 is an inline-exports instance exporting core function 0 as `answer`.
fn inline_answer() -> Vec<u8> {
    vec![
        SECTION_CORE_INSTANCE,
        12,
        1,
        INLINE_EXPORTS,
        1,
        6,
        b'a',
        b'n',
        b's',
        b'w',
        b'e',
        b'r',
        SORT_FUNC,
        0,
    ]
}

/// A component that lifts `answer` of the answer module, with the given type, canon, and
/// export sections.
fn answer_component(types: &[u8], canon: &[u8], export: &[u8]) -> Vec<u8> {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    bytes.extend(alias_answer());
    bytes.extend_from_slice(types);
    bytes.extend_from_slice(canon);
    bytes.extend_from_slice(export);
    bytes
}

fn engine() -> Engine {
    Engine::new(tpt_wasm_runtime::Config::default()).expect("engine")
}

/// A core module that imports `env.add`, a function of two `i32` returning an `i32`, and
/// exports `run`, which calls it with 2 and 3.
fn host_user_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f],
    ));
    bytes.extend(section(
        2,
        &[1, 3, b'e', b'n', b'v', 3, b'a', b'd', b'd', 0, 0],
    ));
    bytes.extend(section(3, &[1, 1]));
    bytes.extend(section(7, &[1, 3, b'r', b'u', b'n', 0, 1]));
    bytes.extend(section(10, &[1, 8, 0, 0x41, 2, 0x41, 3, 0x10, 0, 0x0b]));
    bytes
}

/// A component that imports `add: (s32, s32) -> s32`, lowers it, instantiates the host
/// user module with it, and exports `run: () -> s32`, which the module computes as
/// `add(2, 3)`.
fn host_component() -> Vec<u8> {
    // Type 0 is (s32, s32) -> s32; type 1 is () -> s32.
    let types = section(
        SECTION_TYPE,
        &[
            2,
            0x40,
            2,
            1,
            b'a',
            VALTYPE_S32,
            1,
            b'b',
            VALTYPE_S32,
            0,
            VALTYPE_S32,
            0x40,
            0,
            0,
            VALTYPE_S32,
        ],
    );
    let import = section(
        SECTION_IMPORT,
        &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 0],
    );
    // Lower component function 0 (the import) into core function 0.
    let lower = section(SECTION_CANON, &[1, CANON_LOWER, SORT_FUNC, 0, 0]);
    // Core instance 0 is the inline "add"; core instance 1 runs the host user with "env"
    // bound to instance 0.
    let instances = section(
        SECTION_CORE_INSTANCE,
        &[
            2,
            0x01,
            1,
            3,
            b'a',
            b'd',
            b'd',
            SORT_FUNC,
            0,
            0x00,
            0,
            1,
            3,
            b'e',
            b'n',
            b'v',
            SORT_CORE_INSTANCE,
            0,
        ],
    );
    // Core function 1 is the export `run` of core instance 1.
    let alias = section(SECTION_ALIAS, &[1, 0, 0, 1, 1, 3, b'r', b'u', b'n']);
    // Component function 1 lifts core function 1 with type 1.
    let lift = section(SECTION_CANON, &[1, CANON_LIFT, SORT_FUNC, 1, 0, 1]);
    let export = section(
        SECTION_EXPORT,
        &[1, 0x00, 3, b'r', b'u', b'n', COMPONENT_SORT_FUNC, 1, 0x00],
    );
    let mut bytes = with(&module_section(&host_user_module()));
    bytes.extend(types);
    bytes.extend(import);
    bytes.extend(lower);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(lift);
    bytes.extend(export);
    bytes
}

/// A core module with linear memory and `realloc`, and two functions over strings:
/// `len(ptr, len) -> i32` returns its length argument, and `greet() -> i32` returns the
/// address of a return area that holds a pointer and length to `"hello"`.
fn string_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // Types: (i32, i32) -> i32; (i32, i32, i32, i32) -> i32; () -> i32.
    bytes.extend(section(
        1,
        &[
            3, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1,
            0x7f,
        ],
    ));
    // Functions: realloc has type 1, len has type 0, greet has type 2.
    bytes.extend(section(3, &[3, 1, 0, 2]));
    // One memory of one page.
    bytes.extend(section(5, &[1, 0x00, 1]));
    // Exports: "memory", "realloc" (function 0), "len" (function 1), "greet" (function 2).
    bytes.extend(section(
        7,
        &[
            4, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0, 7, b'r', b'e', b'a', b'l', b'l', b'o',
            b'c', 0, 0, 3, b'l', b'e', b'n', 0, 1, 5, b'g', b'r', b'e', b'e', b't', 0, 2,
        ],
    ));
    // Bodies: realloc returns 1024; len returns its second parameter; greet returns 2048.
    bytes.extend(section(
        10,
        &[
            3, 5, 0x00, 0x41, 0x80, 0x08, 0x0b, 4, 0x00, 0x20, 0x01, 0x0b, 5, 0x00, 0x41, 0x80,
            0x10, 0x0b,
        ],
    ));
    // Data at 2048: a pointer to "hello" (at 2056) and its length, then "hello".
    bytes.extend(section(
        11,
        &[
            1, 0x00, 0x41, 0x80, 0x10, 0x0b, 13, 0x08, 0x08, 0, 0, 5, 0, 0, 0, b'h', b'e', b'l',
            b'l', b'o',
        ],
    ));
    bytes
}

/// A component over `string_module`. Its `len` lifts with the given canon entry, and
/// `greet` lifts with a memory, so the string result can be read.
fn string_component(len_canon: &[u8]) -> Vec<u8> {
    // Types: 0 is (string) -> s32; 1 is () -> string.
    let types = section(
        SECTION_TYPE,
        &[
            2,
            0x40,
            1,
            1,
            b's',
            VALTYPE_STRING,
            0,
            VALTYPE_S32,
            0x40,
            0,
            0,
            VALTYPE_STRING,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    // Core functions: realloc (0), len (1), greet (2). Core memory: memory (0).
    let alias = section(
        SECTION_ALIAS,
        &[
            4, 0, 0, 1, 0, 7, b'r', b'e', b'a', b'l', b'l', b'o', b'c', 0, 0, 1, 0, 3, b'l', b'e',
            b'n', 0, 0, 1, 0, 5, b'g', b'r', b'e', b'e', b't', 0, 2, 1, 0, 6, b'm', b'e', b'm',
            b'o', b'r', b'y',
        ],
    );
    // Canon: lift len with `len_canon`; lift greet with the memory option.
    let mut canon = vec![2];
    canon.extend_from_slice(len_canon);
    canon.extend_from_slice(&[CANON_LIFT, SORT_FUNC, 2, 1, OPTION_MEMORY, 0, 1]);
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[
            2,
            0x00,
            3,
            b'l',
            b'e',
            b'n',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            5,
            b'g',
            b'r',
            b'e',
            b'e',
            b't',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&string_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

/// The canon entry that lifts `len` with a memory and a realloc, which is complete.
fn complete_len_canon() -> Vec<u8> {
    vec![
        CANON_LIFT,
        SORT_FUNC,
        1,
        2,
        OPTION_MEMORY,
        0,
        OPTION_REALLOC,
        0,
        0,
    ]
}

#[test]
fn a_preamble_alone_is_an_empty_component() {
    let component = Component::decode(&PREAMBLE).expect("decodes");
    assert!(component.core_modules().is_empty());
    assert!(component.core_instances().is_empty());
    assert!(component.core_functions().is_empty());
    assert!(component.core_memories().is_empty());
    assert!(component.component_types().is_empty());
    assert!(component.component_exports().is_empty());
}

#[test]
fn a_core_module_section_is_collected_with_its_bytes() {
    let component = Component::decode(&with(&module_section(&EMPTY_MODULE))).expect("decodes");
    assert_eq!(component.core_modules(), [EMPTY_MODULE.to_vec()]);
}

#[test]
fn custom_sections_are_skipped_and_modules_keep_their_order() {
    let mut bytes = with(&[SECTION_CUSTOM, 3, 0x61, 0x62, 0x63]);
    bytes.extend(module_section(&EMPTY_MODULE));
    bytes.extend(module_section(&ANSWER_MODULE));
    let component = Component::decode(&bytes).expect("decodes");
    assert_eq!(component.core_modules().len(), 2);
    assert_eq!(component.core_modules()[1], ANSWER_MODULE.to_vec());
}

#[test]
fn a_core_module_is_not_a_component() {
    assert_eq!(
        Component::decode(&EMPTY_MODULE),
        Err(ComponentError::NotAComponent)
    );
}

#[test]
fn the_wrong_magic_or_layer_is_not_a_component() {
    let mut bad_magic = PREAMBLE;
    bad_magic[0] = 0x01;
    assert_eq!(
        Component::decode(&bad_magic),
        Err(ComponentError::NotAComponent)
    );
    let mut bad_layer = PREAMBLE;
    bad_layer[6] = 0x00;
    assert_eq!(
        Component::decode(&bad_layer),
        Err(ComponentError::NotAComponent)
    );
    assert_eq!(Component::decode(&[]), Err(ComponentError::NotAComponent));
}

#[test]
fn a_section_kind_not_yet_implemented_is_refused_by_id() {
    // Id 4 is the nested component section.
    assert_eq!(
        Component::decode(&with(&[4, 0])),
        Err(ComponentError::UnsupportedSection { id: 4, offset: 8 })
    );
}

#[test]
fn a_section_that_runs_past_the_end_is_truncated() {
    assert_eq!(
        Component::decode(&with(&[SECTION_CORE_MODULE, 9, 0, 0])),
        Err(ComponentError::Truncated { offset: 10 })
    );
}

#[test]
fn an_unterminated_size_is_truncated() {
    assert_eq!(
        Component::decode(&with(&[SECTION_CORE_MODULE, 0x80])),
        Err(ComponentError::Truncated { offset: 10 })
    );
}

#[test]
fn a_size_past_u32_is_an_invalid_integer() {
    assert_eq!(
        Component::decode(&with(&[SECTION_CORE_MODULE, 0x80, 0x80, 0x80, 0x80, 0x10])),
        Err(ComponentError::InvalidInteger { offset: 9 })
    );
}

#[test]
fn an_instantiate_of_a_nested_module_is_decoded() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    let component = Component::decode(&bytes).expect("decodes");
    assert_eq!(
        component.core_instances(),
        [CoreInstance::Instantiate {
            module: 0,
            arguments: Vec::new(),
        }]
    );
}

#[test]
fn an_instance_naming_a_missing_module_is_refused() {
    let bytes = with(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 3, 0]);
    assert_eq!(
        Component::decode(&bytes),
        Err(ComponentError::NoSuchModule { module: 3 })
    );
}

#[test]
fn an_instance_section_with_leftover_bytes_is_malformed() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 5, 0, 0xff, 0xff, 0xff, 0xff]);
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::Malformed { .. })
    ));
}

#[test]
fn an_alias_of_a_core_export_defines_a_core_function() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    bytes.extend(alias_answer());
    let component = Component::decode(&bytes).expect("decodes");
    assert_eq!(
        component.core_functions(),
        [CoreFunction::Alias {
            instance: 0,
            export: "answer".to_owned(),
        }]
    );
}

#[test]
fn an_alias_of_an_undeclared_instance_is_refused() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(alias_answer());
    assert_eq!(
        Component::decode(&bytes),
        Err(ComponentError::NoSuchInstance { instance: 0 })
    );
}

#[test]
fn an_alias_of_another_core_sort_is_refused() {
    // A table: core sort 1 instead of function or memory.
    let bytes = with(&[
        SECTION_ALIAS,
        6,
        1,
        SORT_CORE_WRAPPER,
        1,
        ALIAS_CORE_EXPORT,
        0,
        0,
    ]);
    assert_eq!(
        Component::decode(&bytes),
        Err(ComponentError::UnsupportedAlias {
            what: "alias of a core sort other than function or memory",
            offset: 11,
        })
    );
}

#[test]
fn an_alias_of_a_memory_defines_a_core_memory() {
    let mut bytes = with(&module_section(&string_module()));
    bytes.extend(section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]));
    bytes.extend(section(
        SECTION_ALIAS,
        &[
            1,
            0,
            SORT_CORE_MEMORY,
            1,
            0,
            6,
            b'm',
            b'e',
            b'm',
            b'o',
            b'r',
            b'y',
        ],
    ));
    let component = Component::decode(&bytes).expect("decodes");
    assert_eq!(
        component.core_memories(),
        [CoreMemory {
            instance: 0,
            export: "memory".to_owned(),
        }]
    );
}

#[test]
fn an_inline_export_of_a_missing_function_is_refused() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    let mut section = inline_answer();
    let last = section.len() - 1;
    section[last] = 3;
    bytes.extend(section);
    assert_eq!(
        Component::decode(&bytes),
        Err(ComponentError::NoSuchFunction { function: 3 })
    );
}

#[test]
fn an_inline_exports_instance_runs_the_function_it_names() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    bytes.extend(alias_answer());
    bytes.extend(inline_answer());
    let component = Component::decode(&bytes).expect("decodes");
    let mut instance = component.instantiate(&engine()).expect("instantiates");
    assert_eq!(instance.core_instance_count(), 2);
    assert_eq!(
        instance.call_core(1, "answer", Vec::new()),
        Ok(vec![Value::I32(42)])
    );
    assert_eq!(
        instance.call_core(0, "answer", Vec::new()),
        Ok(vec![Value::I32(42)])
    );
    assert!(matches!(
        instance.call_core(1, "missing", Vec::new()),
        Err(ComponentError::Call { instance: 1, .. })
    ));
}

#[test]
fn an_import_binds_to_an_inline_export() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(module_section(&RELAY_MODULE));
    bytes.extend(instantiate_module_zero());
    bytes.extend(alias_answer());
    bytes.extend(inline_answer());
    // Core instance 2 instantiates module 1 (relay) with "env" bound to instance 1.
    bytes.extend_from_slice(&[
        SECTION_CORE_INSTANCE,
        10,
        1,
        INSTANTIATE,
        1,
        1,
        3,
        b'e',
        b'n',
        b'v',
        SORT_CORE_INSTANCE,
        1,
    ]);
    let component = Component::decode(&bytes).expect("decodes");
    let mut instance = component.instantiate(&engine()).expect("instantiates");
    assert_eq!(
        instance.call_core(2, "relay", Vec::new()),
        Ok(vec![Value::I32(42)])
    );
}

#[test]
fn an_inline_export_of_a_name_the_source_lacks_fails_at_instantiation() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    // An alias to "missing", which the module does not export.
    bytes.extend_from_slice(&[
        SECTION_ALIAS,
        13,
        1,
        SORT_CORE_WRAPPER,
        SORT_FUNC,
        ALIAS_CORE_EXPORT,
        0,
        7,
        b'm',
        b'i',
        b's',
        b's',
        b'i',
        b'n',
        b'g',
    ]);
    bytes.extend(inline_answer());
    let component = Component::decode(&bytes).expect("decodes");
    let error = component
        .instantiate(&engine())
        .expect_err("the export is missing");
    assert!(matches!(
        error,
        ComponentError::Instantiate { instance: 1, .. }
    ));
}

#[test]
fn an_argument_naming_a_later_instance_is_refused() {
    let mut bytes = with(&module_section(&RELAY_MODULE));
    // Module 0 with "env" bound to core instance 5, which is not declared yet.
    bytes.extend_from_slice(&[
        SECTION_CORE_INSTANCE,
        10,
        1,
        INSTANTIATE,
        0,
        1,
        3,
        b'e',
        b'n',
        b'v',
        SORT_CORE_INSTANCE,
        5,
    ]);
    assert_eq!(
        Component::decode(&bytes),
        Err(ComponentError::NoSuchInstance { instance: 5 })
    );
}

#[test]
fn a_duplicate_argument_name_is_malformed() {
    let mut bytes = with(&module_section(&RELAY_MODULE));
    bytes.extend(instantiate_module_zero());
    let env = [3, b'e', b'n', b'v', SORT_CORE_INSTANCE, 0];
    let mut section = vec![SECTION_CORE_INSTANCE, 16, 1, INSTANTIATE, 0, 2];
    section.extend_from_slice(&env);
    section.extend_from_slice(&env);
    bytes.extend_from_slice(&section);
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::Malformed { .. })
    ));
}

#[test]
fn an_unresolved_import_fails_to_instantiate() {
    let mut bytes = with(&module_section(&RELAY_MODULE));
    bytes.extend(instantiate_module_zero());
    let component = Component::decode(&bytes).expect("decodes");
    let error = component
        .instantiate(&engine())
        .expect_err("the import is missing");
    assert!(matches!(
        error,
        ComponentError::Instantiate { instance: 0, .. }
    ));
}

#[test]
fn a_nested_module_instantiates_and_its_export_runs() {
    let mut bytes = with(&module_section(&ANSWER_MODULE));
    bytes.extend(instantiate_module_zero());
    let component = Component::decode(&bytes).expect("decodes");
    let mut instance = component.instantiate(&engine()).expect("instantiates");
    assert_eq!(instance.core_instance_count(), 1);
    assert_eq!(
        instance.call_core(0, "answer", Vec::new()),
        Ok(vec![Value::I32(42)])
    );
    assert!(matches!(
        instance.call_core(1, "answer", Vec::new()),
        Err(ComponentError::NoSuchInstance { instance: 1 })
    ));
}

#[test]
fn a_component_with_no_instances_instantiates_to_nothing() {
    let component = Component::decode(&with(&module_section(&EMPTY_MODULE))).expect("decodes");
    let instance = component.instantiate(&engine()).expect("instantiates");
    assert_eq!(instance.core_instance_count(), 0);
    assert!(instance.export_names().is_empty());
}

#[test]
fn a_lifted_export_returns_a_typed_value() {
    let bytes = answer_component(&TYPE_NO_ARGUMENTS_S32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
    let component = Component::decode(&bytes).expect("decodes");
    assert_eq!(component.component_exports()[0].name, "answer");
    let mut instance = component.instantiate(&engine()).expect("instantiates");
    assert_eq!(instance.export_names(), ["answer"]);
    assert_eq!(
        instance.call_export("answer", Vec::new()),
        Ok(vec![ComponentValue::S32(42)])
    );
}

#[test]
fn the_same_bits_lift_to_the_declared_signedness() {
    let bytes = answer_component(&TYPE_NO_ARGUMENTS_U32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
    let mut instance = Component::decode(&bytes)
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("answer", Vec::new()),
        Ok(vec![ComponentValue::U32(42)])
    );
}

#[test]
fn a_result_of_the_wrong_core_type_is_a_lift_mismatch() {
    let bytes = answer_component(&TYPE_NO_ARGUMENTS_S64, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
    let error = Component::decode(&bytes)
        .expect("decodes")
        .instantiate(&engine())
        .expect_err("i64 is not i32");
    assert!(matches!(
        error,
        ComponentError::LiftMismatch { ref export, .. } if export == "answer"
    ));
}

#[test]
fn a_missing_result_is_a_lift_mismatch() {
    let bytes = answer_component(&TYPE_NO_RESULT, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
    let error = Component::decode(&bytes)
        .expect("decodes")
        .instantiate(&engine())
        .expect_err("the module returns a value");
    assert!(matches!(error, ComponentError::LiftMismatch { .. }));
}

#[test]
fn calls_check_the_argument_count_and_the_export_name() {
    let bytes = answer_component(&TYPE_NO_ARGUMENTS_S32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
    let mut instance = Component::decode(&bytes)
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export("answer", vec![ComponentValue::S32(1)]),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
    assert!(matches!(
        instance.call_export("missing", Vec::new()),
        Err(ComponentError::NoSuchExport { .. })
    ));
}

#[test]
fn canon_options_are_refused_and_a_lower_needs_an_import() {
    // A lift with one option that is not a string encoding, memory, realloc, or post-return: async.
    let with_options = [SECTION_CANON, 6, 1, CANON_LIFT, SORT_FUNC, 0, 1, 0x06];
    assert!(matches!(
        Component::decode(&answer_component(
            &TYPE_NO_ARGUMENTS_S32,
            &with_options,
            &[]
        )),
        Err(ComponentError::UnsupportedCanon {
            what:
                "a canonical option other than a string encoding, memory, realloc, or post-return",
            ..
        })
    ));
    // With no functions defined, a lower of function 0 names nothing.
    let lower = [SECTION_CANON, 5, 1, CANON_LOWER, SORT_FUNC, 0, 0];
    assert_eq!(
        Component::decode(&answer_component(&TYPE_NO_ARGUMENTS_S32, &lower, &[])),
        Err(ComponentError::NoSuchComponentFunction { function: 0 })
    );
}

#[test]
fn a_type_outside_the_supported_set_is_refused() {
    // error-context, a primitive type this crate does not support yet.
    let bool_result = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, 0x64];
    assert!(matches!(
        Component::decode(&answer_component(&bool_result, &CANON_LIFT_ANSWER, &[])),
        Err(ComponentError::UnsupportedType { .. })
    ));
}

#[test]
fn an_export_of_another_sort_or_a_missing_function_is_refused() {
    let value_sort = [
        SECTION_EXPORT,
        12,
        1,
        0x00,
        6,
        b'a',
        b'n',
        b's',
        b'w',
        b'e',
        b'r',
        0x02,
        0,
        0x00,
    ];
    assert!(matches!(
        Component::decode(&answer_component(
            &TYPE_NO_ARGUMENTS_S32,
            &CANON_LIFT_ANSWER,
            &value_sort
        )),
        Err(ComponentError::UnsupportedExport { .. })
    ));
    let missing_function = [
        SECTION_EXPORT,
        12,
        1,
        0x00,
        6,
        b'a',
        b'n',
        b's',
        b'w',
        b'e',
        b'r',
        COMPONENT_SORT_FUNC,
        5,
        0x00,
    ];
    assert_eq!(
        Component::decode(&answer_component(
            &TYPE_NO_ARGUMENTS_S32,
            &CANON_LIFT_ANSWER,
            &missing_function
        )),
        Err(ComponentError::NoSuchComponentFunction { function: 5 })
    );
}

#[test]
fn an_import_with_attributes_or_another_sort_is_refused() {
    let with_attributes = section(SECTION_IMPORT, &[1, 0x02, 3, b'a', b'd', b'd', 0, 0]);
    assert!(matches!(
        Component::decode(&with(&with_attributes)),
        Err(ComponentError::UnsupportedImport { .. })
    ));
    let instance_sort = section(SECTION_IMPORT, &[1, 0x00, 3, b'a', b'd', b'd', 0x05, 0]);
    assert!(matches!(
        Component::decode(&with(&instance_sort)),
        Err(ComponentError::UnsupportedImport { .. })
    ));
}

#[test]
fn an_export_of_an_imported_function_is_refused() {
    let types = section(SECTION_TYPE, &[1, 0x40, 0, 0, VALTYPE_S32]);
    let import = section(
        SECTION_IMPORT,
        &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 0],
    );
    let export = section(
        SECTION_EXPORT,
        &[1, 0x00, 3, b'a', b'd', b'd', COMPONENT_SORT_FUNC, 0, 0x00],
    );
    let mut bytes = with(&types);
    bytes.extend(import);
    bytes.extend(export);
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::UnsupportedExport {
            what: "export of an imported function",
            ..
        })
    ));
}

#[test]
fn lifting_a_lowered_function_is_refused() {
    let types = section(SECTION_TYPE, &[1, 0x40, 0, 0, VALTYPE_S32]);
    let import = section(
        SECTION_IMPORT,
        &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 0],
    );
    let lower = section(SECTION_CANON, &[1, CANON_LOWER, SORT_FUNC, 0, 0]);
    let lift_lowered = section(SECTION_CANON, &[1, CANON_LIFT, SORT_FUNC, 0, 0, 0]);
    let mut bytes = with(&types);
    bytes.extend(import);
    bytes.extend(lower);
    bytes.extend(lift_lowered);
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::UnsupportedCanon {
            what: "lifting a lowered function",
            ..
        })
    ));
}

#[test]
fn a_lowered_import_calls_the_embedder_function() {
    let bytes = host_component();
    let component = Component::decode(&bytes).expect("decodes");
    assert_eq!(component.component_functions().len(), 2);
    let mut imports = granted("add").0;
    imports.define_function("add", add_type(), |args| match args {
        [ComponentValue::S32(a), ComponentValue::S32(b)] => Ok(vec![ComponentValue::S32(a + b)]),
        _ => Err("add takes two s32 values".to_owned()),
    });
    let mut instance = component
        .instantiate_with(&engine(), &imports)
        .expect("instantiates");
    // The module computes add(2, 3) through the lowered import.
    assert_eq!(
        instance.call_export("run", Vec::new()),
        Ok(vec![ComponentValue::S32(5)])
    );
}

/// Imports that draw on a capability set in which the import `name` is granted the `call`
/// operation. The set is returned too, so a test can revoke the grant.
fn granted(name: &str) -> (Imports, Arc<Mutex<CapabilitySet>>) {
    let mut set = CapabilitySet::default();
    set.register(tpt_wasm_capability::CapabilityDescriptor {
        id: CapabilityId::new(name),
        operations: vec![Operation::execute(CALL_OPERATION)],
    })
    .expect("registers");
    set.grant(tpt_wasm_capability::CapabilityGrant {
        capability: CapabilityId::new(name),
        permissions: tpt_wasm_capability::Permissions {
            execute: true,
            ..tpt_wasm_capability::Permissions::default()
        },
    })
    .expect("grants");
    let set = Arc::new(Mutex::new(set));
    (Imports::with_capabilities(set.clone()), set)
}

#[test]
fn an_import_without_a_grant_is_refused_before_anything_runs() {
    let component = Component::decode(&host_component()).expect("decodes");
    let (mut imports, set) = granted("add");
    set.lock().expect("lock").revoke(&CapabilityId::new("add"));
    imports.define_function("add", add_type(), |_| Ok(vec![ComponentValue::S32(0)]));
    let error = component
        .instantiate_with(&engine(), &imports)
        .expect_err("the import is not granted");
    assert!(matches!(
        error,
        ComponentError::CapabilityDenied { ref name, .. } if name == "add"
    ));
}

#[test]
fn an_import_with_no_capability_set_is_refused() {
    let component = Component::decode(&host_component()).expect("decodes");
    let mut imports = Imports::new();
    imports.define_function("add", add_type(), |_| Ok(vec![ComponentValue::S32(0)]));
    let error = component
        .instantiate_with(&engine(), &imports)
        .expect_err("nothing is granted without a set");
    assert!(matches!(error, ComponentError::CapabilityDenied { .. }));
}

#[test]
fn a_revoked_import_stops_the_next_call() {
    let component = Component::decode(&host_component()).expect("decodes");
    let (mut imports, set) = granted("add");
    imports.define_function("add", add_type(), |args| match args {
        [ComponentValue::S32(a), ComponentValue::S32(b)] => Ok(vec![ComponentValue::S32(a + b)]),
        _ => Err("add takes two s32 values".to_owned()),
    });
    let mut instance = component
        .instantiate_with(&engine(), &imports)
        .expect("instantiates");
    assert_eq!(
        instance.call_export("run", Vec::new()),
        Ok(vec![ComponentValue::S32(5)])
    );
    set.lock().expect("lock").revoke(&CapabilityId::new("add"));
    assert!(matches!(
        instance.call_export("run", Vec::new()),
        Err(ComponentError::Call { .. })
    ));
}

/// The type of the host component's import `add`: (s32, s32) -> s32.
fn add_type() -> ComponentFuncType {
    ComponentFuncType {
        params: vec![
            ComponentType::Scalar(Scalar::S32),
            ComponentType::Scalar(Scalar::S32),
        ],
        result: Some(ComponentType::Scalar(Scalar::S32)),
    }
}

#[test]
fn a_supplied_import_of_another_type_is_refused_before_anything_runs() {
    let component = Component::decode(&host_component()).expect("decodes");
    let mut imports = Imports::new();
    // The component declares (s32, s32) -> s32; this supplies (s64) -> s64.
    imports.define_function(
        "add",
        ComponentFuncType {
            params: vec![ComponentType::Scalar(Scalar::S64)],
            result: Some(ComponentType::Scalar(Scalar::S64)),
        },
        |_| Ok(vec![ComponentValue::S64(0)]),
    );
    let error = component
        .instantiate_with(&engine(), &imports)
        .expect_err("the type differs");
    assert!(matches!(
        error,
        ComponentError::ImportMismatch { ref name, .. } if name == "add"
    ));
}

#[test]
fn a_missing_import_is_refused_before_anything_runs() {
    let component = Component::decode(&host_component()).expect("decodes");
    let error = component
        .instantiate(&engine())
        .expect_err("the import is not supplied");
    assert_eq!(
        error,
        ComponentError::MissingImport {
            name: "add".to_owned()
        }
    );
}

#[test]
fn an_embedder_function_error_stops_the_call() {
    let component = Component::decode(&host_component()).expect("decodes");
    let mut imports = granted("add").0;
    imports.define_function("add", add_type(), |_| Err("refused".to_owned()));
    let mut instance = component
        .instantiate_with(&engine(), &imports)
        .expect("instantiates");
    assert!(matches!(
        instance.call_export("run", Vec::new()),
        Err(ComponentError::Call { .. })
    ));
}

#[test]
fn a_string_crosses_into_and_out_of_linear_memory() {
    let bytes = string_component(&complete_len_canon());
    let component = Component::decode(&bytes).expect("decodes");
    let mut instance = component.instantiate(&engine()).expect("instantiates");
    // The argument is copied in through realloc and the module returns its length.
    assert_eq!(
        instance.call_export("len", vec![ComponentValue::String("hello".to_owned())]),
        Ok(vec![ComponentValue::S32(5)])
    );
    assert_eq!(
        instance.call_export("len", vec![ComponentValue::String(String::new())]),
        Ok(vec![ComponentValue::S32(0)])
    );
    // The result is read out of memory, from the pointer and length the module returns.
    assert_eq!(
        instance.call_export("greet", Vec::new()),
        Ok(vec![ComponentValue::String("hello".to_owned())])
    );
}

#[test]
fn a_string_parameter_without_realloc_is_refused() {
    // Memory only: a string parameter has nowhere to be copied to.
    let memory_only = [CANON_LIFT, SORT_FUNC, 1, 1, OPTION_MEMORY, 0, 0];
    assert!(matches!(
        Component::decode(&string_component(&memory_only)),
        Err(ComponentError::UnsupportedCanon {
            what: "a string or list parameter without a realloc option",
            ..
        })
    ));
}

#[test]
fn a_string_without_memory_is_refused() {
    // No options at all: the string result has no memory to be read from.
    let no_options = [CANON_LIFT, SORT_FUNC, 1, 0, 0];
    assert!(matches!(
        Component::decode(&string_component(&no_options)),
        Err(ComponentError::UnsupportedCanon {
            what: "a string, list, or indirect result without a memory option",
            ..
        })
    ));
}

#[test]
fn a_string_crosses_on_the_compiled_backend_too() {
    // The baseline backend keeps its own memory, so the host's writes and reads must go
    // through it as well.
    let config = tpt_wasm_runtime::Config {
        engine_mode: tpt_wasm_runtime::EngineMode::Baseline,
        ..tpt_wasm_runtime::Config::default()
    };
    let engine = Engine::new(config).expect("engine");
    let bytes = string_component(&complete_len_canon());
    let component = Component::decode(&bytes).expect("decodes");
    let mut instance = component.instantiate(&engine).expect("instantiates");
    assert_eq!(
        instance.call_export("len", vec![ComponentValue::String("hello".to_owned())]),
        Ok(vec![ComponentValue::S32(5)])
    );
    assert_eq!(
        instance.call_export("greet", Vec::new()),
        Ok(vec![ComponentValue::String("hello".to_owned())])
    );
}

#[test]
fn a_string_argument_is_checked_against_its_type() {
    let bytes = string_component(&complete_len_canon());
    let mut instance = Component::decode(&bytes)
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export("len", vec![ComponentValue::S32(5)]),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

/// A component whose `answer` returns `valtype`, lifted from the answer module's 42.
fn answer_returning(valtype: u8) -> Vec<u8> {
    answer_component(
        &[SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, valtype],
        &CANON_LIFT_ANSWER,
        &EXPORT_ANSWER,
    )
}

#[test]
fn the_small_and_boolean_types_lift_from_their_core_value() {
    assert_eq!(
        lift(Value::I32(2), Scalar::Bool),
        Ok(ComponentValue::Bool(true))
    );
    assert_eq!(
        lift(Value::I32(0), Scalar::Bool),
        Ok(ComponentValue::Bool(false))
    );
    // The unsigned types keep the low bits; the signed ones sign-extend them.
    assert_eq!(
        lift(Value::I32(0x1ff), Scalar::U8),
        Ok(ComponentValue::U8(0xff))
    );
    assert_eq!(
        lift(Value::I32(0xff), Scalar::S8),
        Ok(ComponentValue::S8(-1))
    );
    assert_eq!(
        lift(Value::I32(0x8000), Scalar::S16),
        Ok(ComponentValue::S16(i16::MIN))
    );
    assert_eq!(
        lift(Value::I32(0x1_ffff), Scalar::U16),
        Ok(ComponentValue::U16(0xffff))
    );
    assert_eq!(
        lift(Value::I32(42), Scalar::Char),
        Ok(ComponentValue::Char('*'))
    );
}

#[test]
fn a_character_that_is_not_a_scalar_value_traps() {
    assert!(lift(Value::I32(0xD800), Scalar::Char).is_err());
    assert!(lift(Value::I32(0xDFFF), Scalar::Char).is_err());
    assert!(lift(Value::I32(0x11_0000), Scalar::Char).is_err());
    assert!(lift(Value::I32(-1), Scalar::Char).is_err());
}

#[test]
fn the_small_and_boolean_types_lower_to_one_i32() {
    assert_eq!(
        lower_scalar(&ComponentValue::Bool(true)),
        Some(Value::I32(1))
    );
    assert_eq!(lower_scalar(&ComponentValue::S8(-1)), Some(Value::I32(-1)));
    assert_eq!(
        lower_scalar(&ComponentValue::U8(255)),
        Some(Value::I32(255))
    );
    assert_eq!(lower_scalar(&ComponentValue::S16(-2)), Some(Value::I32(-2)));
    assert_eq!(
        lower_scalar(&ComponentValue::U16(65535)),
        Some(Value::I32(65535))
    );
    assert_eq!(
        lower_scalar(&ComponentValue::Char('A')),
        Some(Value::I32(65))
    );
}

#[test]
fn exports_return_the_small_and_boolean_types() {
    let cases = [
        (VALTYPE_BOOL, ComponentValue::Bool(true)),
        (VALTYPE_U8, ComponentValue::U8(42)),
        (VALTYPE_S8, ComponentValue::S8(42)),
        (VALTYPE_U16, ComponentValue::U16(42)),
        (VALTYPE_S16, ComponentValue::S16(42)),
        (VALTYPE_CHAR, ComponentValue::Char('*')),
    ];
    for (valtype, expected) in cases {
        let mut instance = Component::decode(&answer_returning(valtype))
            .expect("decodes")
            .instantiate(&engine())
            .expect("instantiates");
        assert_eq!(
            instance.call_export("answer", Vec::new()),
            Ok(vec![expected]),
            "valtype {valtype:#x}"
        );
    }
}

/// Every truncation and every single-byte corruption of the fixtures is decoded, and every
/// one that decodes is instantiated. None may panic: the bytes are untrusted, and a panic
/// here is a denial of service in the embedder.
#[test]
fn no_truncation_or_corruption_of_a_component_panics() {
    let engine = engine();
    let fixtures = [
        host_component(),
        answer_component(&TYPE_NO_ARGUMENTS_S32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER),
        string_component(&complete_len_canon()),
        record_component(&complete_rec_canon()),
        list_component(&complete_count_canon()),
        option_component(),
        enum_component(),
        flags_component(),
        tuple_component(),
        result_component(),
        variant_component(),
        wide_component(&seventeen_u32_params(), &WIDE_OPTIONS),
    ];
    let mut decoded = 0usize;
    let mut instantiated = 0usize;
    for fixture in &fixtures {
        for length in 0..fixture.len() {
            let _ = Component::decode(&fixture[..length]);
        }
        for position in 0..fixture.len() {
            for mask in [0x01u8, 0x40, 0x80, 0xff] {
                let mut bytes = fixture.clone();
                bytes[position] ^= mask;
                let Ok(component) = Component::decode(&bytes) else {
                    continue;
                };
                decoded += 1;
                let mut imports = granted("add").0;
                imports.define_function("add", add_type(), |args| match args {
                    [ComponentValue::S32(a), ComponentValue::S32(b)] => {
                        Ok(vec![ComponentValue::S32(a.wrapping_add(*b))])
                    }
                    _ => Err("add takes two s32 values".to_owned()),
                });
                if let Ok(mut instance) = component.instantiate_with(&engine, &imports) {
                    instantiated += 1;
                    let names: Vec<String> = instance
                        .export_names()
                        .into_iter()
                        .map(str::to_owned)
                        .collect();
                    for name in names {
                        let _ = instance.call_export(&name, Vec::new());
                        let _ = instance
                            .call_export(&name, vec![ComponentValue::String("hi".to_owned())]);
                    }
                }
            }
        }
    }
    assert!(decoded > 0, "some corruptions must still decode");
    assert!(instantiated > 0, "some corruptions must still instantiate");
}

#[test]
fn scalars_lower_and_lift_by_their_bits() {
    assert_eq!(
        lower_scalar(&ComponentValue::U32(u32::MAX)),
        Some(Value::I32(-1))
    );
    assert_eq!(
        lift(Value::I32(-1), Scalar::U32),
        Ok(ComponentValue::U32(u32::MAX))
    );
    assert_eq!(
        lift(Value::I32(-1), Scalar::S32),
        Ok(ComponentValue::S32(-1))
    );
    // A NaN with a payload keeps its bits through the round trip.
    let nan = f32::from_bits(0x7fc0_1234);
    let lowered = lower_scalar(&ComponentValue::F32(nan)).expect("a scalar lowers");
    assert_eq!(lowered, Value::F32(0x7fc0_1234));
    assert_eq!(
        lift(lowered, Scalar::F32).map(|value| match value {
            ComponentValue::F32(value) => value.to_bits(),
            _ => 0,
        }),
        Ok(0x7fc0_1234)
    );
    assert!(lift(Value::I64(1), Scalar::S32).is_err());
    assert_eq!(lower_scalar(&ComponentValue::String("x".to_owned())), None);
}

/// A core module with memory, and two functions over a record of `x: u32` and `y: u8`:
/// `sum(x, y) -> i32` adds its two parameters, and `rec() -> i32` returns the address of a record
/// that holds `x = 42` and `y = 7`, in the canonical layout.
fn record_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f],
    ));
    bytes.extend(section(3, &[2, 0, 1]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            3, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0, 3, b's', b'u', b'm', 0, 0, 3, b'r',
            b'e', b'c', 0, 1,
        ],
    ));
    bytes.extend(section(
        10,
        &[
            2, 7, 0, 0x20, 0, 0x20, 1, 0x6a, 0x0b, 5, 0, 0x41, 0x80, 0x10, 0x0b,
        ],
    ));
    // The record at 2048: x = 42 at offset 0, y = 7 at offset 4, then padding to 8 bytes.
    bytes.extend(section(
        11,
        &[1, 0x00, 0x41, 0x80, 0x10, 0x0b, 8, 42, 0, 0, 0, 7, 0, 0, 0],
    ));
    bytes
}

/// A component over `record_module`: a record type `{x: u32, y: u8}`, `sum` lifted with the
/// record as its parameter, and `rec` lifted with the record as its result. `rec_canon` is the
/// canon entry for `rec`.
fn record_component(rec_canon: &[u8]) -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            3,
            TYPE_RECORD,
            2,
            1,
            b'x',
            VALTYPE_U32,
            1,
            b'y',
            VALTYPE_U8,
            TYPE_FUNC,
            1,
            1,
            b'r',
            0x00,
            0x00,
            VALTYPE_U32,
            TYPE_FUNC,
            0,
            0x00,
            0x00,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            3, 0, 0, 1, 0, 3, b's', b'u', b'm', 0, 0, 1, 0, 3, b'r', b'e', b'c', 0, 2, 1, 0, 6,
            b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    // Canon: lift `sum` with no options; lift `rec` with the given options.
    let mut canon = vec![2, CANON_LIFT, SORT_FUNC, 0, 0, 1];
    canon.extend_from_slice(rec_canon);
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[
            2,
            0x00,
            3,
            b's',
            b'u',
            b'm',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            3,
            b'r',
            b'e',
            b'c',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&record_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

/// The canon entry that lifts `rec` through the memory, which the record result needs.
fn complete_rec_canon() -> Vec<u8> {
    vec![CANON_LIFT, SORT_FUNC, 1, 1, OPTION_MEMORY, 0, 2]
}

/// A record value `{x, y}` of the record type.
fn point(x: u32, y: u8) -> ComponentValue {
    ComponentValue::Record(vec![
        ("x".to_owned(), ComponentValue::U32(x)),
        ("y".to_owned(), ComponentValue::U8(y)),
    ])
}

#[test]
fn a_record_parameter_is_passed_as_its_flat_fields() {
    let mut instance = Component::decode(&record_component(&complete_rec_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("sum", vec![point(40, 2)]),
        Ok(vec![ComponentValue::U32(42)])
    );
}

#[test]
fn a_record_result_is_read_back_from_memory_by_the_canonical_layout() {
    let mut instance = Component::decode(&record_component(&complete_rec_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("rec", Vec::new()),
        Ok(vec![point(42, 7)])
    );
}

#[test]
fn a_record_argument_with_the_wrong_fields_is_refused() {
    let mut instance = Component::decode(&record_component(&complete_rec_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    let renamed = ComponentValue::Record(vec![
        ("a".to_owned(), ComponentValue::U32(1)),
        ("y".to_owned(), ComponentValue::U8(1)),
    ]);
    assert!(matches!(
        instance.call_export("sum", vec![renamed]),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn a_record_result_without_memory_is_refused() {
    // No options: the record flattens to two values, so it is returned through memory.
    let bytes = record_component(&[CANON_LIFT, SORT_FUNC, 1, 0, 2]);
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::UnsupportedCanon { .. })
    ));
}

#[test]
fn an_import_with_a_record_in_its_type_is_refused() {
    let types = section(
        SECTION_TYPE,
        &[
            2,
            TYPE_RECORD,
            1,
            1,
            b'x',
            VALTYPE_U32,
            TYPE_FUNC,
            1,
            1,
            b'r',
            0x00,
            0x00,
            VALTYPE_U32,
        ],
    );
    let import = section(
        SECTION_IMPORT,
        &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 1],
    );
    let mut bytes = with(&types);
    bytes.extend(import);
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::UnsupportedImport {
            what: "import with a record or list in its type",
            ..
        })
    ));
}

#[test]
fn a_record_is_laid_out_by_the_canonical_alignment_rules() {
    let point = [
        RecordField {
            name: "x".to_owned(),
            ty: Scalar::U32,
        },
        RecordField {
            name: "y".to_owned(),
            ty: Scalar::U8,
        },
    ];
    assert_eq!(record_layout(&point), (vec![0, 4], 8, 4));
    let mixed = [
        RecordField {
            name: "a".to_owned(),
            ty: Scalar::U8,
        },
        RecordField {
            name: "b".to_owned(),
            ty: Scalar::U64,
        },
    ];
    assert_eq!(record_layout(&mixed), (vec![0, 8], 16, 8));
}

/// A core module with memory and `realloc`, and three functions over lists: `first(ptr, len)`
/// returns the first byte, `count(ptr, len)` returns the length, and `items() -> i32` returns the
/// address of a list of two `u32`s, 7 and 9, in the canonical layout.
fn list_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[
            3, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f, 0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1,
            0x7f,
        ],
    ));
    // Functions: first and count have type 0, items has type 1, realloc has type 2.
    bytes.extend(section(3, &[4, 0, 0, 1, 2]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            5, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0, 5, b'f', b'i', b'r', b's', b't', 0, 0,
            5, b'c', b'o', b'u', b'n', b't', 0, 1, 5, b'i', b't', b'e', b'm', b's', 0, 2, 7, b'r',
            b'e', b'a', b'l', b'l', b'o', b'c', 0, 3,
        ],
    ));
    bytes.extend(section(
        10,
        &[
            4, 7, 0, 0x20, 0, 0x2d, 0, 0, 0x0b, 4, 0, 0x20, 1, 0x0b, 5, 0, 0x41, 0x80, 0x10, 0x0b,
            5, 0, 0x41, 0x80, 0x08, 0x0b,
        ],
    ));
    // The list at 2048: a pointer to the elements (2056) and their count (2), then 7 and 9.
    bytes.extend(section(
        11,
        &[
            1, 0x00, 0x41, 0x80, 0x10, 0x0b, 16, 0x08, 0x08, 0, 0, 2, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0,
            0,
        ],
    ));
    bytes
}

/// A component over `list_module`: `first` and `count` take a `list<u8>` and `items` returns a
/// `list<u32>`. `count_canon` is the canon entry for `count`.
fn list_component(count_canon: &[u8]) -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            4,
            TYPE_LIST,
            VALTYPE_U8,
            TYPE_LIST,
            VALTYPE_U32,
            TYPE_FUNC,
            1,
            1,
            b'l',
            0x00,
            0x00,
            VALTYPE_U32,
            TYPE_FUNC,
            0,
            0x00,
            0x01,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            5, 0, 0, 1, 0, 5, b'f', b'i', b'r', b's', b't', 0, 0, 1, 0, 5, b'c', b'o', b'u', b'n',
            b't', 0, 0, 1, 0, 5, b'i', b't', b'e', b'm', b's', 0, 0, 1, 0, 7, b'r', b'e', b'a',
            b'l', b'l', b'o', b'c', 0, 2, 1, 0, 6, b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    // Canon: `first` and `count` lift with memory and realloc, `items` with memory only.
    let mut canon = vec![
        3,
        CANON_LIFT,
        SORT_FUNC,
        0,
        2,
        OPTION_MEMORY,
        0,
        OPTION_REALLOC,
        3,
        2,
    ];
    canon.extend_from_slice(count_canon);
    canon.extend_from_slice(&[CANON_LIFT, SORT_FUNC, 2, 1, OPTION_MEMORY, 0, 3]);
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[
            3,
            0x00,
            5,
            b'f',
            b'i',
            b'r',
            b's',
            b't',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            5,
            b'c',
            b'o',
            b'u',
            b'n',
            b't',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
            0x00,
            5,
            b'i',
            b't',
            b'e',
            b'm',
            b's',
            COMPONENT_SORT_FUNC,
            2,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&list_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

/// The canon entry for `count` with memory and realloc, which is complete.
fn complete_count_canon() -> Vec<u8> {
    vec![
        CANON_LIFT,
        SORT_FUNC,
        1,
        2,
        OPTION_MEMORY,
        0,
        OPTION_REALLOC,
        3,
        2,
    ]
}

#[test]
fn a_list_argument_is_copied_into_memory_and_read_by_the_module() {
    let mut instance = Component::decode(&list_component(&complete_count_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    let list = ComponentValue::List(vec![ComponentValue::U8(5), ComponentValue::U8(6)]);
    // `first` reads the first byte at the pointer the list was copied to.
    assert_eq!(
        instance.call_export("first", vec![list.clone()]),
        Ok(vec![ComponentValue::U32(5)])
    );
    assert_eq!(
        instance.call_export("count", vec![list]),
        Ok(vec![ComponentValue::U32(2)])
    );
}

#[test]
fn an_empty_list_argument_has_length_zero() {
    let mut instance = Component::decode(&list_component(&complete_count_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("count", vec![ComponentValue::List(Vec::new())]),
        Ok(vec![ComponentValue::U32(0)])
    );
}

#[test]
fn a_list_result_is_read_back_from_memory_by_the_canonical_layout() {
    let mut instance = Component::decode(&list_component(&complete_count_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("items", Vec::new()),
        Ok(vec![ComponentValue::List(vec![
            ComponentValue::U32(7),
            ComponentValue::U32(9),
        ])])
    );
}

#[test]
fn a_list_element_of_another_type_is_refused() {
    let mut instance = Component::decode(&list_component(&complete_count_canon()))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export(
            "count",
            vec![ComponentValue::List(vec![ComponentValue::U16(1)])]
        ),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn a_list_parameter_without_realloc_is_refused() {
    // Memory only: a list parameter has nowhere to be copied to.
    let memory_only = [CANON_LIFT, SORT_FUNC, 1, 1, OPTION_MEMORY, 0, 2];
    assert!(matches!(
        Component::decode(&list_component(&memory_only)),
        Err(ComponentError::UnsupportedCanon {
            what: "a string or list parameter without a realloc option",
            ..
        })
    ));
}

/// A core module with memory and two functions over an option of `u32`: `has(disc, payload)`
/// returns the discriminant, and `opt() -> i32` returns the address of an option holding 7, in the
/// canonical layout (a discriminant byte, then the payload at its alignment).
fn option_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f],
    ));
    bytes.extend(section(3, &[2, 0, 1]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            3, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0, 3, b'h', b'a', b's', 0, 0, 3, b'o',
            b'p', b't', 0, 1,
        ],
    ));
    bytes.extend(section(
        10,
        &[2, 4, 0, 0x20, 0, 0x0b, 5, 0, 0x41, 0x80, 0x10, 0x0b],
    ));
    // The option at 2048: discriminant 1, three bytes of padding, then the payload 7.
    bytes.extend(section(
        11,
        &[1, 0x00, 0x41, 0x80, 0x10, 0x0b, 8, 1, 0, 0, 0, 7, 0, 0, 0],
    ));
    bytes
}

/// A component over `option_module`: `has` takes an `option<u32>` and returns `u32`, and `opt`
/// returns an `option<u32>`.
fn option_component() -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            3,
            TYPE_OPTION,
            VALTYPE_U32,
            TYPE_FUNC,
            1,
            1,
            b'o',
            0x00,
            0x00,
            VALTYPE_U32,
            TYPE_FUNC,
            0,
            0x00,
            0x00,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            3, 0, 0, 1, 0, 3, b'h', b'a', b's', 0, 0, 1, 0, 3, b'o', b'p', b't', 0, 2, 1, 0, 6,
            b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    // `has` needs no memory: its option is passed directly. `opt` returns through memory.
    let canon = section(
        SECTION_CANON,
        &[
            2,
            CANON_LIFT,
            SORT_FUNC,
            0,
            0,
            1,
            CANON_LIFT,
            SORT_FUNC,
            1,
            1,
            OPTION_MEMORY,
            0,
            2,
        ],
    );
    let export = section(
        SECTION_EXPORT,
        &[
            2,
            0x00,
            3,
            b'h',
            b'a',
            b's',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            3,
            b'o',
            b'p',
            b't',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&option_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

#[test]
fn an_option_argument_passes_its_discriminant_and_payload() {
    let mut instance = Component::decode(&option_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    // The module returns the discriminant: 1 for some, 0 for none.
    assert_eq!(
        instance.call_export(
            "has",
            vec![ComponentValue::Option(Some(Box::new(ComponentValue::U32(
                7
            ))))]
        ),
        Ok(vec![ComponentValue::U32(1)])
    );
    assert_eq!(
        instance.call_export("has", vec![ComponentValue::Option(None)]),
        Ok(vec![ComponentValue::U32(0)])
    );
}

#[test]
fn an_option_result_is_read_back_from_memory() {
    let mut instance = Component::decode(&option_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("opt", Vec::new()),
        Ok(vec![ComponentValue::Option(Some(Box::new(
            ComponentValue::U32(7)
        )))])
    );
}

#[test]
fn an_option_payload_of_another_type_is_refused() {
    let mut instance = Component::decode(&option_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export(
            "has",
            vec![ComponentValue::Option(Some(Box::new(ComponentValue::U8(
                1
            ))))]
        ),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn an_option_of_a_string_is_refused() {
    let bytes = with(&section(SECTION_TYPE, &[1, TYPE_OPTION, VALTYPE_STRING]));
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::UnsupportedType {
            what: "an option of a string",
            ..
        })
    ));
}

/// A core module with two functions over an enum's case index: `echo(i) -> i32` returns its
/// argument, and `three() -> i32` returns 3, which is not a case of a three-case enum.
fn enum_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(1, &[2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]));
    bytes.extend(section(3, &[2, 0, 1]));
    bytes.extend(section(
        7,
        &[
            2, 4, b'e', b'c', b'h', b'o', 0, 0, 5, b't', b'h', b'r', b'e', b'e', 0, 1,
        ],
    ));
    bytes.extend(section(10, &[2, 4, 0, 0x20, 0, 0x0b, 4, 0, 0x41, 3, 0x0b]));
    bytes
}

/// A component over `enum_module`: an enum of the cases `red`, `green`, and `blue`; `echo` takes
/// and returns it, and `three` returns it too.
fn enum_component() -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            3, TYPE_ENUM, 3, 3, b'r', b'e', b'd', 5, b'g', b'r', b'e', b'e', b'n', 4, b'b', b'l',
            b'u', b'e', TYPE_FUNC, 1, 1, b'c', 0x00, 0x00, 0x00, TYPE_FUNC, 0, 0x00, 0x00,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            2, 0, 0, 1, 0, 4, b'e', b'c', b'h', b'o', 0, 0, 1, 0, 5, b't', b'h', b'r', b'e', b'e',
        ],
    );
    let canon = section(
        SECTION_CANON,
        &[
            2, CANON_LIFT, SORT_FUNC, 0, 0, 1, CANON_LIFT, SORT_FUNC, 1, 0, 2,
        ],
    );
    let export = section(
        SECTION_EXPORT,
        &[
            2,
            0x00,
            4,
            b'e',
            b'c',
            b'h',
            b'o',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            5,
            b't',
            b'h',
            b'r',
            b'e',
            b'e',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&enum_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

#[test]
fn an_enum_passes_its_case_index_in_both_directions() {
    let mut instance = Component::decode(&enum_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("echo", vec![ComponentValue::Enum("green".to_owned())]),
        Ok(vec![ComponentValue::Enum("green".to_owned())])
    );
}

#[test]
fn an_enum_result_outside_its_cases_is_a_call_error() {
    let mut instance = Component::decode(&enum_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    // The module returns 3, which names no case of a three-case enum.
    assert!(matches!(
        instance.call_export("three", Vec::new()),
        Err(ComponentError::Call { .. })
    ));
}

#[test]
fn an_enum_argument_that_is_not_a_case_is_refused() {
    let mut instance = Component::decode(&enum_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export("echo", vec![ComponentValue::Enum("purple".to_owned())]),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn an_enum_with_a_repeated_case_is_malformed() {
    let bytes = with(&section(SECTION_TYPE, &[1, TYPE_ENUM, 2, 1, b'a', 1, b'a']));
    assert!(matches!(
        Component::decode(&bytes),
        Err(ComponentError::Malformed { .. })
    ));
}

/// A component over `enum_module` whose type is a set of the flags `read`, `write`, and `exec`:
/// `echo` takes and returns the set, and `three` returns the bits of the module's `three`, which
/// is 8, a bit that names no flag.
fn flags_component() -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            3, TYPE_FLAGS, 3, 4, b'r', b'e', b'a', b'd', 5, b'w', b'r', b'i', b't', b'e', 4, b'e',
            b'x', b'e', b'c', TYPE_FUNC, 1, 1, b'f', 0x00, 0x00, 0x00, TYPE_FUNC, 0, 0x00, 0x00,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            2, 0, 0, 1, 0, 4, b'e', b'c', b'h', b'o', 0, 0, 1, 0, 5, b't', b'h', b'r', b'e', b'e',
        ],
    );
    let canon = section(
        SECTION_CANON,
        &[
            2, CANON_LIFT, SORT_FUNC, 0, 0, 1, CANON_LIFT, SORT_FUNC, 1, 0, 2,
        ],
    );
    let export = section(
        SECTION_EXPORT,
        &[
            2,
            0x00,
            4,
            b'e',
            b'c',
            b'h',
            b'o',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            5,
            b't',
            b'h',
            b'r',
            b'e',
            b'e',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
        ],
    );
    // The `three` export of the enum module returns 3; here the module is the same, and `three`
    // is checked through a set whose bit is 3 (value 8) by the test below.
    let mut bytes = with(&module_section(&flags_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

#[test]
fn a_set_of_flags_passes_its_bits_in_both_directions() {
    let mut instance = Component::decode(&flags_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    let set = ComponentValue::Flags(vec!["read".to_owned(), "exec".to_owned()]);
    assert_eq!(
        instance.call_export("echo", vec![set.clone()]),
        Ok(vec![set])
    );
    assert_eq!(
        instance.call_export("echo", vec![ComponentValue::Flags(Vec::new())]),
        Ok(vec![ComponentValue::Flags(Vec::new())])
    );
}

#[test]
fn a_flag_that_is_not_a_label_is_refused() {
    let mut instance = Component::decode(&flags_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export("echo", vec![ComponentValue::Flags(vec!["nope".to_owned()])]),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn flags_with_more_than_32_labels_are_malformed() {
    let mut content = vec![1, TYPE_FLAGS, 33];
    for index in 0..33u8 {
        content.push(1);
        content.push(b'a' + index % 26);
    }
    assert!(matches!(
        Component::decode(&with(&section(SECTION_TYPE, &content))),
        Err(ComponentError::Malformed { .. })
    ));
}

/// A core module like `enum_module`, but whose `three` returns 8: a set of three flags has no bit 3.
fn flags_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(1, &[2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]));
    bytes.extend(section(3, &[2, 0, 1]));
    bytes.extend(section(
        7,
        &[
            2, 4, b'e', b'c', b'h', b'o', 0, 0, 5, b't', b'h', b'r', b'e', b'e', 0, 1,
        ],
    ));
    bytes.extend(section(10, &[2, 4, 0, 0x20, 0, 0x0b, 4, 0, 0x41, 8, 0x0b]));
    bytes
}

#[test]
fn a_flags_result_with_a_bit_beyond_the_labels_is_a_call_error() {
    let mut instance = Component::decode(&flags_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    // The module returns 8, which sets bit 3; the set has three labels, so bit 3 names no flag.
    assert!(matches!(
        instance.call_export("three", Vec::new()),
        Err(ComponentError::Call { .. })
    ));
}

/// A component over `record_module` whose types are a tuple of `u32` and `u8`: `sum` takes the
/// tuple, and `rec` returns it. The tuple lays out as the record does.
fn tuple_component() -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            3,
            TYPE_TUPLE,
            2,
            VALTYPE_U32,
            VALTYPE_U8,
            TYPE_FUNC,
            1,
            1,
            b't',
            0x00,
            0x00,
            VALTYPE_U32,
            TYPE_FUNC,
            0,
            0x00,
            0x00,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            3, 0, 0, 1, 0, 3, b's', b'u', b'm', 0, 0, 1, 0, 3, b'r', b'e', b'c', 0, 2, 1, 0, 6,
            b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    let mut canon = vec![2, CANON_LIFT, SORT_FUNC, 0, 0, 1];
    canon.extend_from_slice(&[CANON_LIFT, SORT_FUNC, 1, 1, OPTION_MEMORY, 0, 2]);
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[
            2,
            0x00,
            3,
            b's',
            b'u',
            b'm',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            3,
            b'r',
            b'e',
            b'c',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&record_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

#[test]
fn a_tuple_parameter_and_result_pass_their_values_in_order() {
    let mut instance = Component::decode(&tuple_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export(
            "sum",
            vec![ComponentValue::Tuple(vec![
                ComponentValue::U32(40),
                ComponentValue::U8(2),
            ])]
        ),
        Ok(vec![ComponentValue::U32(42)])
    );
    assert_eq!(
        instance.call_export("rec", Vec::new()),
        Ok(vec![ComponentValue::Tuple(vec![
            ComponentValue::U32(42),
            ComponentValue::U8(7),
        ])])
    );
}

#[test]
fn a_tuple_with_the_wrong_arity_is_refused() {
    let mut instance = Component::decode(&tuple_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert!(matches!(
        instance.call_export(
            "sum",
            vec![ComponentValue::Tuple(vec![ComponentValue::U32(1)])]
        ),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn an_empty_tuple_is_malformed_and_a_string_in_a_tuple_is_refused() {
    assert!(matches!(
        Component::decode(&with(&section(SECTION_TYPE, &[1, TYPE_TUPLE, 0]))),
        Err(ComponentError::Malformed { .. })
    ));
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[1, TYPE_TUPLE, 1, VALTYPE_STRING]
        ))),
        Err(ComponentError::UnsupportedType {
            what: "a string in a tuple",
            ..
        })
    ));
}

/// A core module with a memory, and four functions. `ok` and `err` return a pointer to a result
/// of `result<u32, u32>` laid out in memory, and `none` returns the discriminant of a result with
/// no payloads. `take` returns the payload of the result passed as its two flat values.
fn result_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // Two types: [] -> [i32], and [i32, i32] -> [i32].
    bytes.extend(section(
        1,
        &[2, 0x60, 0, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7f],
    ));
    bytes.extend(section(3, &[4, 0, 0, 0, 1]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            5, 2, b'o', b'k', 0, 0, 3, b'e', b'r', b'r', 0, 1, 4, b'n', b'o', b'n', b'e', 0, 2, 4,
            b't', b'a', b'k', b'e', 0, 3, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0,
        ],
    ));
    bytes.extend(section(
        10,
        &[
            4, 4, 0, 0x41, 16, 0x0b, 4, 0, 0x41, 24, 0x0b, 4, 0, 0x41, 1, 0x0b, 4, 0, 0x20, 1, 0x0b,
        ],
    ));
    // At 16, an ok with 42: discriminant 0, the payload at offset 4. At 24, an err with 7.
    bytes.extend(section(
        11,
        &[
            1, 0, 0x41, 16, 0x0b, 16, 0, 0, 0, 0, 42, 0, 0, 0, 1, 0, 0, 0, 7, 0, 0, 0,
        ],
    ));
    bytes
}

/// A component over `result_module`, with the types `result<u32, u32>`, a function returning it,
/// a payload-free result, and `take`, a function from `result<u32, u32>` to `u32`.
fn result_component() -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            5,
            TYPE_RESULT,
            1,
            VALTYPE_U32,
            1,
            VALTYPE_U32,
            TYPE_FUNC,
            0,
            0x00,
            0,
            TYPE_RESULT,
            0,
            0,
            TYPE_FUNC,
            0,
            0x00,
            2,
            TYPE_FUNC,
            1,
            1,
            b'x',
            0x00,
            0x00,
            VALTYPE_U32,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            5, 0, 0, 1, 0, 2, b'o', b'k', 0, 0, 1, 0, 3, b'e', b'r', b'r', 0, 0, 1, 0, 4, b'n',
            b'o', b'n', b'e', 0, 0, 1, 0, 4, b't', b'a', b'k', b'e', 0, 2, 1, 0, 6, b'm', b'e',
            b'm', b'o', b'r', b'y',
        ],
    );
    let canon = section(
        SECTION_CANON,
        &[
            4,
            CANON_LIFT,
            SORT_FUNC,
            0,
            1,
            OPTION_MEMORY,
            0,
            1,
            CANON_LIFT,
            SORT_FUNC,
            1,
            1,
            OPTION_MEMORY,
            0,
            1,
            CANON_LIFT,
            SORT_FUNC,
            2,
            0,
            3,
            CANON_LIFT,
            SORT_FUNC,
            3,
            0,
            4,
        ],
    );
    let export = section(
        SECTION_EXPORT,
        &[
            4,
            0x00,
            2,
            b'o',
            b'k',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            3,
            b'e',
            b'r',
            b'r',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
            0x00,
            4,
            b'n',
            b'o',
            b'n',
            b'e',
            COMPONENT_SORT_FUNC,
            2,
            0x00,
            0x00,
            4,
            b't',
            b'a',
            b'k',
            b'e',
            COMPONENT_SORT_FUNC,
            3,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&result_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

#[test]
fn a_result_with_payloads_is_read_back_from_memory() {
    let mut instance = Component::decode(&result_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("ok", Vec::new()),
        Ok(vec![ComponentValue::Result(Ok(Some(Box::new(
            ComponentValue::U32(42)
        ))))])
    );
    assert_eq!(
        instance.call_export("err", Vec::new()),
        Ok(vec![ComponentValue::Result(Err(Some(Box::new(
            ComponentValue::U32(7)
        ))))])
    );
}

#[test]
fn a_payload_free_result_is_a_direct_discriminant() {
    let mut instance = Component::decode(&result_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    // The module returns 1, the discriminant of the error case, which has no payload.
    assert_eq!(
        instance.call_export("none", Vec::new()),
        Ok(vec![ComponentValue::Result(Err(None))])
    );
}

#[test]
fn a_result_argument_passes_its_discriminant_and_payload() {
    let mut instance = Component::decode(&result_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export(
            "take",
            vec![ComponentValue::Result(Ok(Some(Box::new(
                ComponentValue::U32(5)
            ))))]
        ),
        Ok(vec![ComponentValue::U32(5)])
    );
    assert_eq!(
        instance.call_export(
            "take",
            vec![ComponentValue::Result(Err(Some(Box::new(
                ComponentValue::U32(9)
            ))))]
        ),
        Ok(vec![ComponentValue::U32(9)])
    );
    // A case without its payload, or a payload of another type, is refused before the call.
    assert!(matches!(
        instance.call_export("take", vec![ComponentValue::Result(Ok(None))]),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
    assert!(matches!(
        instance.call_export(
            "take",
            vec![ComponentValue::Result(Ok(Some(Box::new(
                ComponentValue::S32(1)
            ))))]
        ),
        Err(ComponentError::ArgumentMismatch { .. })
    ));
}

#[test]
fn a_result_with_a_string_or_a_compound_payload_is_refused() {
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[1, TYPE_RESULT, 1, VALTYPE_STRING, 0]
        ))),
        Err(ComponentError::UnsupportedType {
            what: "a string in a result",
            ..
        })
    ));
    // Type 0 is a list, so the result's payload is a compound type.
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[2, TYPE_LIST, VALTYPE_U8, TYPE_RESULT, 1, 0x00, 0]
        ))),
        Err(ComponentError::UnsupportedType {
            what: "a compound type in a result",
            ..
        })
    ));
}

#[test]
fn a_result_whose_payloads_differ_in_flat_type_is_refused() {
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[1, TYPE_RESULT, 1, VALTYPE_U32, 1, VALTYPE_F32]
        ))),
        Err(ComponentError::UnsupportedType {
            what: "a result whose payloads have different flat types",
            ..
        })
    ));
}

#[test]
fn a_result_case_with_an_unknown_tag_is_malformed() {
    assert!(matches!(
        Component::decode(&with(&section(SECTION_TYPE, &[1, TYPE_RESULT, 2]))),
        Err(ComponentError::Malformed { .. })
    ));
}

/// A core module with a memory and three functions. `make` returns a pointer to a variant in
/// memory, the `count` case holding 42. `take` returns the payload of the variant passed as its
/// two flat values. `pick` returns the discriminant of a payload-free variant's second case.
fn variant_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[2, 0x60, 0, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7f],
    ));
    bytes.extend(section(3, &[3, 0, 1, 0]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            4, 4, b'm', b'a', b'k', b'e', 0, 0, 4, b't', b'a', b'k', b'e', 0, 1, 4, b'p', b'i',
            b'c', b'k', 0, 2, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0,
        ],
    ));
    bytes.extend(section(
        10,
        &[
            3, 4, 0, 0x41, 16, 0x0b, 4, 0, 0x20, 1, 0x0b, 4, 0, 0x41, 1, 0x0b,
        ],
    ));
    // At 16: discriminant 1 (`count`), then the payload 42 at offset 4.
    bytes.extend(section(
        11,
        &[1, 0, 0x41, 16, 0x0b, 8, 1, 0, 0, 0, 42, 0, 0, 0],
    ));
    bytes
}

/// A component over `variant_module`: `variant { empty, count(u32), flag }` returned by `make`,
/// `variant { a, b }` returned by `pick`, and `take`, from the first variant to `u32`.
fn variant_component() -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            5,
            TYPE_VARIANT,
            3,
            5,
            b'e',
            b'm',
            b'p',
            b't',
            b'y',
            0,
            0,
            5,
            b'c',
            b'o',
            b'u',
            b'n',
            b't',
            1,
            VALTYPE_U32,
            0,
            4,
            b'f',
            b'l',
            b'a',
            b'g',
            0,
            0,
            TYPE_FUNC,
            0,
            0x00,
            0,
            TYPE_VARIANT,
            2,
            1,
            b'a',
            0,
            0,
            1,
            b'b',
            0,
            0,
            TYPE_FUNC,
            0,
            0x00,
            2,
            TYPE_FUNC,
            1,
            1,
            b'x',
            0x00,
            0x00,
            VALTYPE_U32,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            4, 0, 0, 1, 0, 4, b'm', b'a', b'k', b'e', 0, 0, 1, 0, 4, b't', b'a', b'k', b'e', 0, 0,
            1, 0, 4, b'p', b'i', b'c', b'k', 0, 2, 1, 0, 6, b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    let canon = section(
        SECTION_CANON,
        &[
            3,
            CANON_LIFT,
            SORT_FUNC,
            0,
            1,
            OPTION_MEMORY,
            0,
            1,
            CANON_LIFT,
            SORT_FUNC,
            2,
            0,
            3,
            CANON_LIFT,
            SORT_FUNC,
            1,
            0,
            4,
        ],
    );
    let export = section(
        SECTION_EXPORT,
        &[
            3,
            0x00,
            4,
            b'm',
            b'a',
            b'k',
            b'e',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            4,
            b'p',
            b'i',
            b'c',
            b'k',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
            0x00,
            4,
            b't',
            b'a',
            b'k',
            b'e',
            COMPONENT_SORT_FUNC,
            2,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&variant_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

#[test]
fn a_variant_with_a_payload_is_read_back_from_memory() {
    let mut instance = Component::decode(&variant_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("make", Vec::new()),
        Ok(vec![ComponentValue::Variant(
            "count".to_owned(),
            Some(Box::new(ComponentValue::U32(42)))
        )])
    );
}

#[test]
fn a_variant_without_payloads_is_a_direct_case_index() {
    let mut instance = Component::decode(&variant_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    // The module returns 1, the second case of `variant { a, b }`.
    assert_eq!(
        instance.call_export("pick", Vec::new()),
        Ok(vec![ComponentValue::Variant("b".to_owned(), None)])
    );
}

#[test]
fn a_variant_argument_passes_its_case_and_payload() {
    let mut instance = Component::decode(&variant_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export(
            "take",
            vec![ComponentValue::Variant(
                "count".to_owned(),
                Some(Box::new(ComponentValue::U32(9)))
            )]
        ),
        Ok(vec![ComponentValue::U32(9)])
    );
    // A case without a payload passes zero in the payload's place.
    assert_eq!(
        instance.call_export(
            "take",
            vec![ComponentValue::Variant("empty".to_owned(), None)]
        ),
        Ok(vec![ComponentValue::U32(0)])
    );
}

#[test]
fn a_variant_argument_that_does_not_match_its_type_is_refused() {
    let mut instance = Component::decode(&variant_component())
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    let refused = [
        ComponentValue::Variant("count".to_owned(), None),
        ComponentValue::Variant("empty".to_owned(), Some(Box::new(ComponentValue::U32(1)))),
        ComponentValue::Variant("nope".to_owned(), None),
        ComponentValue::Variant("count".to_owned(), Some(Box::new(ComponentValue::S32(1)))),
    ];
    for argument in refused {
        assert!(matches!(
            instance.call_export("take", vec![argument]),
            Err(ComponentError::ArgumentMismatch { .. })
        ));
    }
}

#[test]
fn a_variant_with_a_string_or_a_repeated_label_is_refused() {
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[1, TYPE_VARIANT, 1, 1, b'a', 0x01, VALTYPE_STRING, 0]
        ))),
        Err(ComponentError::UnsupportedType {
            what: "a string in a variant",
            ..
        })
    ));
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[1, TYPE_VARIANT, 2, 1, b'a', 0, 0, 1, b'a', 0, 0]
        ))),
        Err(ComponentError::Malformed { .. })
    ));
}

#[test]
fn a_variant_whose_payloads_differ_in_flat_type_or_have_no_cases_is_refused() {
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[
                1,
                TYPE_VARIANT,
                2,
                1,
                b'a',
                1,
                VALTYPE_U32,
                0,
                1,
                b'b',
                1,
                VALTYPE_F32,
                0
            ]
        ))),
        Err(ComponentError::UnsupportedType {
            what: "a variant whose payloads have different flat types",
            ..
        })
    ));
    assert!(matches!(
        Component::decode(&with(&section(SECTION_TYPE, &[1, TYPE_VARIANT, 0]))),
        Err(ComponentError::Malformed { .. })
    ));
    // A refines field other than `0x00` is malformed.
    assert!(matches!(
        Component::decode(&with(&section(
            SECTION_TYPE,
            &[1, TYPE_VARIANT, 1, 1, b'a', 0, 1]
        ))),
        Err(ComponentError::Malformed { .. })
    ));
}

/// A core module whose `sum` takes a pointer to seventeen `u32` values in memory and returns their
/// total, and whose `realloc` returns the address 1024, where the block is then read.
fn wide_module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[
            2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f,
        ],
    ));
    bytes.extend(section(3, &[2, 0, 1]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            3, 3, b's', b'u', b'm', 0, 0, 7, b'r', b'e', b'a', b'l', b'l', b'o', b'c', 0, 1, 6,
            b'm', b'e', b'm', b'o', b'r', b'y', 2, 0,
        ],
    ));
    // sum: the first field, then each later one added on: local.get 0, i32.load align 2 offset.
    let mut body = vec![0x00, 0x20, 0x00, 0x28, 0x02, 0x00];
    for field in 1..17u8 {
        body.extend_from_slice(&[0x20, 0x00, 0x28, 0x02, field * 4, 0x6a]);
    }
    body.push(0x0b);
    let mut code = vec![2, body.len() as u8];
    code.extend(&body);
    code.extend_from_slice(&[5, 0, 0x41, 0x80, 0x08, 0x0b]);
    bytes.extend(section(10, &code));
    bytes
}

/// The parameter list of seventeen `u32` values, named `a` through `q`.
fn seventeen_u32_params() -> Vec<u8> {
    let mut params = vec![17];
    for name in b'a'..b'a' + 17 {
        params.extend_from_slice(&[1, name, VALTYPE_U32]);
    }
    params
}

/// A component over `wide_module` with one lifted function of the given parameters, returning a
/// `u32`, and the given canonical options on its lift.
fn wide_component(params: &[u8], options: &[u8]) -> Vec<u8> {
    let mut type_body = vec![1, TYPE_FUNC];
    type_body.extend_from_slice(params);
    type_body.extend_from_slice(&[0x00, VALTYPE_U32]);
    let types = section(SECTION_TYPE, &type_body);
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            3, 0, 0, 1, 0, 3, b's', b'u', b'm', 0, 0, 1, 0, 7, b'r', b'e', b'a', b'l', b'l', b'o',
            b'c', 0, 2, 1, 0, 6, b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    let mut canon = vec![1, CANON_LIFT, SORT_FUNC, 0];
    canon.extend_from_slice(options);
    canon.push(0);
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[1, 0x00, 3, b's', b'u', b'm', COMPONENT_SORT_FUNC, 0, 0x00],
    );
    let mut bytes = with(&module_section(&wide_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

/// The canonical options of a lift with a memory and a realloc.
const WIDE_OPTIONS: [u8; 5] = [2, OPTION_MEMORY, 0, OPTION_REALLOC, 1];

#[test]
fn seventeen_parameters_are_stored_as_one_block_behind_a_pointer() {
    let mut instance = Component::decode(&wide_component(&seventeen_u32_params(), &WIDE_OPTIONS))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    let arguments: Vec<ComponentValue> = (1..=17).map(ComponentValue::U32).collect();
    assert_eq!(
        instance.call_export("sum", arguments),
        Ok(vec![ComponentValue::U32(153)])
    );
}

#[test]
fn indirect_parameters_need_a_realloc_and_may_not_hold_a_string() {
    assert!(matches!(
        Component::decode(&wide_component(
            &seventeen_u32_params(),
            &[1, OPTION_MEMORY, 0]
        )),
        Err(ComponentError::UnsupportedCanon { .. })
    ));
    let mut with_string = vec![17, 1, b'a', VALTYPE_STRING];
    for name in b'b'..b'b' + 16 {
        with_string.extend_from_slice(&[1, name, VALTYPE_U32]);
    }
    assert!(matches!(
        Component::decode(&wide_component(&with_string, &WIDE_OPTIONS)),
        Err(ComponentError::UnsupportedType {
            what: "a string or list among more than 16 flat parameters",
            ..
        })
    ));
}

#[test]
fn an_import_with_more_than_16_flat_parameters_is_refused() {
    let types = section(
        SECTION_TYPE,
        &[
            &[1, TYPE_FUNC][..],
            &seventeen_u32_params()[..],
            &[0x00, VALTYPE_U32][..],
        ]
        .concat(),
    );
    let import = section(SECTION_IMPORT, &[1, 0x00, 1, b'f', 0x01, 0]);
    let canon = section(SECTION_CANON, &[1, CANON_LOWER, SORT_FUNC, 0, 0]);
    assert!(matches!(
        Component::decode(&with(&[types, import, canon].concat())),
        Err(ComponentError::UnsupportedCanon { .. })
    ));
}

#[test]
fn a_record_and_an_option_are_stored_at_their_canonical_offsets() {
    let defined = vec![ComponentTypeDef::Record(vec![
        RecordField {
            name: "a".to_owned(),
            ty: Scalar::U8,
        },
        RecordField {
            name: "b".to_owned(),
            ty: Scalar::U32,
        },
    ])];
    let mut buffer = [0u8; 8];
    store(
        &ComponentValue::Record(vec![
            ("a".to_owned(), ComponentValue::U8(7)),
            ("b".to_owned(), ComponentValue::U32(0x0102_0304)),
        ]),
        &ComponentType::Record(0),
        &defined,
        &mut buffer,
        0,
    )
    .expect("stores");
    assert_eq!(buffer, [7, 0, 0, 0, 4, 3, 2, 1]);

    let defined = vec![ComponentTypeDef::Option(Scalar::U32)];
    let mut buffer = [0u8; 8];
    store(
        &ComponentValue::Option(Some(Box::new(ComponentValue::U32(5)))),
        &ComponentType::Option(0),
        &defined,
        &mut buffer,
        0,
    )
    .expect("stores");
    assert_eq!(buffer, [1, 0, 0, 0, 5, 0, 0, 0]);
}

/// A core module whose exports return string headers laid out in its memory: `a` at 16 for
/// UTF-16 `hé`, `b` at 24 for Latin-1 `hé!`, `c` at 32 for UTF-16 `h€` with the tag bit, and `d` at
/// 72 for a lone surrogate. `measure` returns the length it is given, and `realloc` returns 1024.
fn text_module() -> Vec<u8> {
    let mut image = vec![0u8; 96];
    let mut place = |at: usize, bytes: &[u8]| image[at..at + bytes.len()].copy_from_slice(bytes);
    place(16, &[40, 0, 0, 0, 2, 0, 0, 0]);
    place(24, &[48, 0, 0, 0, 3, 0, 0, 0]);
    place(32, &[56, 0, 0, 0, 2, 0, 0, 0x80]);
    place(72, &[80, 0, 0, 0, 1, 0, 0, 0]);
    place(40, &[0x68, 0, 0xE9, 0]);
    place(48, &[0x68, 0xE9, 0x21]);
    place(56, &[0x68, 0, 0xAC, 0x20]);
    place(80, &[0x00, 0xD8]);

    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[
            3, 0x60, 0, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1,
            0x7f,
        ],
    ));
    bytes.extend(section(3, &[6, 0, 0, 0, 0, 1, 2]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            7, 1, b'a', 0, 0, 1, b'b', 0, 1, 1, b'c', 0, 2, 1, b'd', 0, 3, 7, b'm', b'e', b'a',
            b's', b'u', b'r', b'e', 0, 4, 7, b'r', b'e', b'a', b'l', b'l', b'o', b'c', 0, 5, 6,
            b'm', b'e', b'm', b'o', b'r', b'y', 2, 0,
        ],
    ));
    bytes.extend(section(
        10,
        &[
            6, 4, 0, 0x41, 16, 0x0b, 4, 0, 0x41, 24, 0x0b, 4, 0, 0x41, 32, 0x0b, 5, 0, 0x41, 0xC8,
            0x00, 0x0b, 4, 0, 0x20, 1, 0x0b, 5, 0, 0x41, 0x80, 0x08, 0x0b,
        ],
    ));
    let mut data = vec![1, 0, 0x41, 0, 0x0b, image.len() as u8];
    data.extend(&image);
    bytes.extend(section(11, &data));
    bytes
}

/// A component over `text_module`: `a`, `b`, `c`, and `d` return a string, and `measure` takes
/// one and returns its length. Every lift takes the given canonical options, which name the
/// memory and `realloc` (core function 5).
fn text_component(options: &[u8]) -> Vec<u8> {
    let types = section(
        SECTION_TYPE,
        &[
            2,
            TYPE_FUNC,
            0,
            0x00,
            VALTYPE_STRING,
            TYPE_FUNC,
            1,
            1,
            b's',
            VALTYPE_STRING,
            0x00,
            VALTYPE_U32,
        ],
    );
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            7, 0, 0, 1, 0, 1, b'a', 0, 0, 1, 0, 1, b'b', 0, 0, 1, 0, 1, b'c', 0, 0, 1, 0, 1, b'd',
            0, 0, 1, 0, 7, b'm', b'e', b'a', b's', b'u', b'r', b'e', 0, 0, 1, 0, 7, b'r', b'e',
            b'a', b'l', b'l', b'o', b'c', 0, 2, 1, 0, 6, b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    let lift = |core: u8, ty: u8| {
        let mut bytes = vec![CANON_LIFT, SORT_FUNC, core];
        bytes.extend_from_slice(options);
        bytes.push(ty);
        bytes
    };
    let mut canon = vec![5];
    canon.extend(lift(0, 0));
    canon.extend(lift(1, 0));
    canon.extend(lift(2, 0));
    canon.extend(lift(3, 0));
    canon.extend(lift(4, 1));
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[
            5,
            0x00,
            1,
            b'a',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
            0x00,
            1,
            b'b',
            COMPONENT_SORT_FUNC,
            1,
            0x00,
            0x00,
            1,
            b'c',
            COMPONENT_SORT_FUNC,
            2,
            0x00,
            0x00,
            1,
            b'd',
            COMPONENT_SORT_FUNC,
            3,
            0x00,
            0x00,
            7,
            b'm',
            b'e',
            b'a',
            b's',
            b'u',
            b'r',
            b'e',
            COMPONENT_SORT_FUNC,
            4,
            0x00,
        ],
    );
    let mut bytes = with(&module_section(&text_module()));
    bytes.extend(types);
    bytes.extend(instances);
    bytes.extend(alias);
    bytes.extend(canon);
    bytes.extend(export);
    bytes
}

/// The canonical options of a `utf16` lift, and of a `latin1+utf16` lift, with memory and realloc.
const UTF16_OPTIONS: [u8; 6] = [3, OPTION_UTF16, OPTION_MEMORY, 0, OPTION_REALLOC, 5];
const LATIN1_OPTIONS: [u8; 6] = [3, OPTION_LATIN1_UTF16, OPTION_MEMORY, 0, OPTION_REALLOC, 5];

fn text_instance(options: &[u8]) -> ComponentInstance {
    Component::decode(&text_component(options))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates")
}

#[test]
fn a_utf16_string_result_is_decoded_from_its_code_units() {
    let mut instance = text_instance(&UTF16_OPTIONS);
    assert_eq!(
        instance.call_export("a", Vec::new()),
        Ok(vec![ComponentValue::String("hé".to_owned())])
    );
}

#[test]
fn an_unpaired_surrogate_in_a_utf16_string_is_a_call_error() {
    let mut instance = text_instance(&UTF16_OPTIONS);
    assert!(matches!(
        instance.call_export("d", Vec::new()),
        Err(ComponentError::Call { .. })
    ));
}

#[test]
fn a_utf16_string_argument_is_copied_as_code_units() {
    let mut instance = text_instance(&UTF16_OPTIONS);
    assert_eq!(
        instance.call_export("measure", vec![ComponentValue::String("hé".to_owned())]),
        Ok(vec![ComponentValue::U32(2)])
    );
    assert_eq!(
        instance
            .read_core_memory(0, "memory", 1024, 4)
            .expect("reads"),
        vec![0x68, 0, 0xE9, 0]
    );
}

#[test]
fn latin1_and_utf16_strings_are_read_by_their_length_tag() {
    let mut instance = text_instance(&LATIN1_OPTIONS);
    assert_eq!(
        instance.call_export("b", Vec::new()),
        Ok(vec![ComponentValue::String("hé!".to_owned())])
    );
    assert_eq!(
        instance.call_export("c", Vec::new()),
        Ok(vec![ComponentValue::String("h€".to_owned())])
    );
}

#[test]
fn a_latin1_string_argument_stays_latin1_and_others_take_the_utf16_tag() {
    let mut instance = text_instance(&LATIN1_OPTIONS);
    assert_eq!(
        instance.call_export("measure", vec![ComponentValue::String("hé".to_owned())]),
        Ok(vec![ComponentValue::U32(2)])
    );
    assert_eq!(
        instance
            .read_core_memory(0, "memory", 1024, 2)
            .expect("reads"),
        vec![0x68, 0xE9]
    );
    assert_eq!(
        instance.call_export("measure", vec![ComponentValue::String("h€".to_owned())]),
        Ok(vec![ComponentValue::U32(0x8000_0002)])
    );
    assert_eq!(
        instance
            .read_core_memory(0, "memory", 1024, 4)
            .expect("reads"),
        vec![0x68, 0, 0xAC, 0x20]
    );
}

#[test]
fn a_lift_names_at_most_one_string_encoding() {
    assert!(matches!(
        Component::decode(&text_component(&[
            4,
            OPTION_UTF16,
            OPTION_LATIN1_UTF16,
            OPTION_MEMORY,
            0,
            OPTION_REALLOC,
            5,
        ])),
        Err(ComponentError::Malformed { .. })
    ));
    assert!(matches!(
        Component::decode(&text_component(&[
            3,
            0x05,
            OPTION_MEMORY,
            0,
            OPTION_REALLOC,
            5
        ])),
        Err(ComponentError::UnsupportedCanon { .. })
    ));
}

/// Corrupting the bytes of the string data image, its headers and its text alike, may never panic.
/// The whole component is too slow to corrupt at every length, so this covers only the image.
#[test]
fn corrupted_string_images_do_not_panic() {
    let engine = engine();
    let fixture = text_component(&LATIN1_OPTIONS);
    // The image starts 16 bytes before the first header, which is `hé`'s pointer and length.
    let header = fixture
        .windows(8)
        .position(|window| window == [40, 0, 0, 0, 2, 0, 0, 0])
        .expect("the first header is in the fixture");
    let image = header - 16;
    for position in image..image + 96 {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut bytes = fixture.clone();
            bytes[position] ^= mask;
            let Ok(component) = Component::decode(&bytes) else {
                continue;
            };
            if let Ok(mut instance) = component.instantiate(&engine) {
                for name in ["a", "b", "c", "d", "measure"] {
                    let _ = instance.call_export(name, Vec::new());
                    let _ =
                        instance.call_export(name, vec![ComponentValue::String("hi".to_owned())]);
                }
            }
        }
    }
}

/// A component over a module whose `pick` returns 7, and whose `post` stores its argument at 2048.
/// The lift of `pick` names core function `post_index` as its post-return.
fn post_component(post_index: u8) -> Vec<u8> {
    post_component_with(&[2, OPTION_MEMORY, 0, OPTION_POST_RETURN, post_index])
}

/// The same component, with the given canonical options on its lift.
fn post_component_with(options: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(1, &[2, 0x60, 0, 1, 0x7f, 0x60, 1, 0x7f, 0]));
    bytes.extend(section(3, &[2, 0, 1]));
    bytes.extend(section(5, &[1, 0x00, 1]));
    bytes.extend(section(
        7,
        &[
            3, 4, b'p', b'i', b'c', b'k', 0, 0, 4, b'p', b'o', b's', b't', 0, 1, 6, b'm', b'e',
            b'm', b'o', b'r', b'y', 2, 0,
        ],
    ));
    bytes.extend(section(
        10,
        &[
            2, 4, 0, 0x41, 7, 0x0b, 10, 0, 0x41, 0x80, 0x10, 0x20, 0, 0x36, 2, 0, 0x0b,
        ],
    ));
    let types = section(SECTION_TYPE, &[1, TYPE_FUNC, 0, 0x00, VALTYPE_U32]);
    let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
    let alias = section(
        SECTION_ALIAS,
        &[
            3, 0, 0, 1, 0, 4, b'p', b'i', b'c', b'k', 0, 0, 1, 0, 4, b'p', b'o', b's', b't', 0, 2,
            1, 0, 6, b'm', b'e', b'm', b'o', b'r', b'y',
        ],
    );
    let mut canon = vec![1, CANON_LIFT, SORT_FUNC, 0];
    canon.extend_from_slice(options);
    canon.push(0);
    let canon = section(SECTION_CANON, &canon);
    let export = section(
        SECTION_EXPORT,
        &[
            1,
            0x00,
            4,
            b'p',
            b'i',
            b'c',
            b'k',
            COMPONENT_SORT_FUNC,
            0,
            0x00,
        ],
    );
    let mut component = with(&module_section(&bytes));
    component.extend(types);
    component.extend(instances);
    component.extend(alias);
    component.extend(canon);
    component.extend(export);
    component
}

#[test]
fn a_post_return_runs_after_the_result_is_read() {
    let mut instance = Component::decode(&post_component(1))
        .expect("decodes")
        .instantiate(&engine())
        .expect("instantiates");
    assert_eq!(
        instance.call_export("pick", Vec::new()),
        Ok(vec![ComponentValue::U32(7)])
    );
    // The post-return was given the value the module returned, and stored it.
    assert_eq!(
        instance
            .read_core_memory(0, "memory", 2048, 4)
            .expect("reads"),
        vec![7, 0, 0, 0]
    );
}

#[test]
fn a_post_return_of_the_wrong_type_is_refused_before_anything_runs() {
    assert!(matches!(
        Component::decode(&post_component(0))
            .expect("decodes")
            .instantiate(&engine()),
        Err(ComponentError::LiftMismatch { .. })
    ));
}

#[test]
fn a_second_post_return_is_malformed_and_one_naming_no_function_is_refused() {
    assert!(matches!(
        Component::decode(&post_component_with(&[
            3,
            OPTION_POST_RETURN,
            1,
            OPTION_POST_RETURN,
            1,
            OPTION_MEMORY,
            0,
        ])),
        Err(ComponentError::Malformed { .. })
    ));
    assert!(matches!(
        Component::decode(&post_component(9)),
        Err(ComponentError::NoSuchFunction { .. })
    ));
}
