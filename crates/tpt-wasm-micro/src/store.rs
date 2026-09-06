// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

use tpt_wasm_types::{FunctionType, GlobalType, RefValue, Value};

/// The WebAssembly store — the global mutable state of a running program.
#[derive(Debug, Default)]
pub struct Store {
    pub memories: Vec<MemoryInstance>,
    pub tables: Vec<TableInstance>,
    pub globals: Vec<GlobalInstance>,
    pub functions: Vec<FuncInstance>,
    pub instances: Vec<Instance>,
}

#[derive(Debug)]
pub struct MemoryInstance {
    pub data: Vec<u8>,
    pub max_pages: Option<u64>,
}

impl MemoryInstance {
    pub fn pages(&self) -> u64 {
        (self.data.len() as u64) / 65536
    }
}

#[derive(Debug)]
pub struct TableInstance {
    pub element_type: tpt_wasm_types::RefType,
    pub elements: Vec<Option<RefValue>>,
    pub max_elements: Option<u32>,
}

#[derive(Debug)]
pub struct GlobalInstance {
    pub global_type: GlobalType,
    pub value: Value,
}

#[derive(Debug)]
pub enum FuncInstance {
    Wasm(WasmFuncInstance),
    Host(HostFuncInstance),
}

#[derive(Debug)]
pub struct WasmFuncInstance {
    pub func_type: FunctionType,
    pub instance_idx: u32,
    pub func_idx: u32,
}

#[derive(Debug)]
pub struct HostFuncInstance {
    pub func_type: FunctionType,
    pub name: String,
}

#[derive(Debug)]
pub struct Instance {
    pub module_types: Vec<FunctionType>,
    pub func_addrs: Vec<u32>,
    pub table_addrs: Vec<u32>,
    pub memory_addrs: Vec<u32>,
    pub global_addrs: Vec<u32>,
}
