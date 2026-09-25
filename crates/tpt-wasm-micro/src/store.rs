// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Safe runtime storage for the Micro interpreter.

use std::fmt;

use tpt_wasm_types::{
    FunctionType, GlobalType, RefType, RefValue, ResourceLimits, Value, ValueType,
};

pub const WASM_PAGE_SIZE: usize = 65_536;

/// Errors returned by checked store operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    InvalidLimits(&'static str),
    LimitExceeded(&'static str),
    UnknownMemory(u32),
    UnknownTable(u32),
    UnknownGlobal(u32),
    UnknownFunction(u32),
    UnknownInstance(u32),
    TypeMismatch {
        expected: ValueType,
        actual: ValueType,
    },
    ImmutableGlobal(u32),
    MemoryOutOfBounds,
    TableOutOfBounds,
    InvalidReferenceType,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for StoreError {}

/// The global mutable state of a running program.
#[derive(Debug, Clone)]
pub struct Store {
    pub memories: Vec<MemoryInstance>,
    pub tables: Vec<TableInstance>,
    pub globals: Vec<GlobalInstance>,
    pub functions: Vec<FuncInstance>,
    pub instances: Vec<Instance>,
    pub limits: ResourceLimits,
}

impl Default for Store {
    fn default() -> Self {
        Self::new(ResourceLimits::default())
    }
}

impl Store {
    pub fn new(limits: ResourceLimits) -> Self {
        Self {
            memories: Vec::new(),
            tables: Vec::new(),
            globals: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            limits,
        }
    }

    pub fn allocate_memory(
        &mut self,
        min_pages: u64,
        max_pages: Option<u64>,
    ) -> Result<u32, StoreError> {
        let memory = MemoryInstance::new(min_pages, max_pages, self.limits.max_memory_pages)?;
        let index = u32::try_from(self.memories.len())
            .map_err(|_| StoreError::LimitExceeded("too many memories"))?;
        self.memories.push(memory);
        Ok(index)
    }

    pub fn allocate_table(
        &mut self,
        element_type: RefType,
        min_elements: u32,
        max_elements: Option<u32>,
    ) -> Result<u32, StoreError> {
        let table = TableInstance::new(
            element_type,
            min_elements,
            max_elements,
            self.limits.max_table_elements,
        )?;
        let index = u32::try_from(self.tables.len())
            .map_err(|_| StoreError::LimitExceeded("too many tables"))?;
        self.tables.push(table);
        Ok(index)
    }

    pub fn add_global(&mut self, global_type: GlobalType, value: Value) -> Result<u32, StoreError> {
        if !value_matches(value_type(&value), global_type.value_type) {
            return Err(StoreError::TypeMismatch {
                expected: global_type.value_type,
                actual: value_type(&value),
            });
        }
        let index = u32::try_from(self.globals.len())
            .map_err(|_| StoreError::LimitExceeded("too many globals"))?;
        self.globals.push(GlobalInstance { global_type, value });
        Ok(index)
    }

    pub fn add_function(&mut self, function: FuncInstance) -> Result<u32, StoreError> {
        let index = u32::try_from(self.functions.len())
            .map_err(|_| StoreError::LimitExceeded("too many functions"))?;
        self.functions.push(function);
        Ok(index)
    }

    pub fn add_instance(&mut self, instance: Instance) -> Result<u32, StoreError> {
        if self.instances.len() >= self.limits.max_instances as usize {
            return Err(StoreError::LimitExceeded("instance limit exceeded"));
        }
        let index = u32::try_from(self.instances.len())
            .map_err(|_| StoreError::LimitExceeded("too many instances"))?;
        self.instances.push(instance);
        Ok(index)
    }

    pub fn add_wasm_function(
        &mut self,
        instance_idx: u32,
        func_idx: u32,
        func_type: FunctionType,
        instrs: Vec<crate::instr::Instr>,
        local_types: Vec<ValueType>,
    ) -> Result<u32, StoreError> {
        self.add_function(FuncInstance::Wasm(WasmFuncInstance {
            func_type,
            instance_idx,
            func_idx,
            instrs,
            local_types,
        }))
    }

    pub fn add_host_function(
        &mut self,
        func_type: FunctionType,
        name: impl Into<String>,
    ) -> Result<u32, StoreError> {
        self.add_function(FuncInstance::Host(HostFuncInstance {
            func_type,
            name: name.into(),
        }))
    }

    pub fn memory(&self, index: u32) -> Result<&MemoryInstance, StoreError> {
        self.memories
            .get(index as usize)
            .ok_or(StoreError::UnknownMemory(index))
    }

    pub fn memory_mut(&mut self, index: u32) -> Result<&mut MemoryInstance, StoreError> {
        self.memories
            .get_mut(index as usize)
            .ok_or(StoreError::UnknownMemory(index))
    }

    pub fn table(&self, index: u32) -> Result<&TableInstance, StoreError> {
        self.tables
            .get(index as usize)
            .ok_or(StoreError::UnknownTable(index))
    }

    pub fn table_mut(&mut self, index: u32) -> Result<&mut TableInstance, StoreError> {
        self.tables
            .get_mut(index as usize)
            .ok_or(StoreError::UnknownTable(index))
    }

    pub fn global(&self, index: u32) -> Result<&GlobalInstance, StoreError> {
        self.globals
            .get(index as usize)
            .ok_or(StoreError::UnknownGlobal(index))
    }

    pub fn global_mut(&mut self, index: u32) -> Result<&mut GlobalInstance, StoreError> {
        self.globals
            .get_mut(index as usize)
            .ok_or(StoreError::UnknownGlobal(index))
    }

    pub fn set_global(&mut self, index: u32, value: Value) -> Result<(), StoreError> {
        let global = self.global_mut(index)?;
        if !global.global_type.mutable {
            return Err(StoreError::ImmutableGlobal(index));
        }
        if !value_matches(value_type(&value), global.global_type.value_type) {
            return Err(StoreError::TypeMismatch {
                expected: global.global_type.value_type,
                actual: value_type(&value),
            });
        }
        global.value = value;
        Ok(())
    }

    pub fn function(&self, index: u32) -> Result<&FuncInstance, StoreError> {
        self.functions
            .get(index as usize)
            .ok_or(StoreError::UnknownFunction(index))
    }

    pub fn instance(&self, index: u32) -> Result<&Instance, StoreError> {
        self.instances
            .get(index as usize)
            .ok_or(StoreError::UnknownInstance(index))
    }
}

fn value_type(value: &Value) -> ValueType {
    match value {
        Value::I32(_) => ValueType::I32,
        Value::I64(_) => ValueType::I64,
        Value::F32(_) => ValueType::F32,
        Value::F64(_) => ValueType::F64,
        Value::V128(_) => ValueType::V128,
        Value::Ref(value) => ValueType::Ref(match value {
            RefValue::Null(kind) => *kind,
            RefValue::FuncRef(_) => RefType::FuncRef,
            RefValue::ExternRef(_) => RefType::ExternRef,
        }),
    }
}

fn value_matches(actual: ValueType, expected: ValueType) -> bool {
    actual == expected
}

#[cfg(test)]
mod tests {
    use super::{RefType, Store, StoreError};
    use tpt_wasm_types::{GlobalType, RefValue, ResourceLimits, Value, ValueType};

    fn limits() -> ResourceLimits {
        ResourceLimits {
            max_memory_pages: 2,
            max_table_elements: 4,
            max_instances: 1,
            max_stack_depth: 8,
            max_call_depth: 2,
            max_execution_steps: None,
        }
    }

    #[test]
    fn memory_allocation_growth_and_bounds_are_checked() {
        let mut store = Store::new(limits());
        let index = store.allocate_memory(1, Some(2)).unwrap();
        assert_eq!(store.memory(index).unwrap().pages(), 1);
        assert_eq!(store.memory_mut(index).unwrap().grow(1, 2).unwrap(), 1);
        assert_eq!(store.memory(index).unwrap().pages(), 2);
        assert_eq!(
            store.memory_mut(index).unwrap().grow(1, 2),
            Err(StoreError::LimitExceeded("memory limit exceeded"))
        );
        assert_eq!(
            store.memory(index).unwrap().read(u64::MAX, 1),
            Err(StoreError::MemoryOutOfBounds)
        );
    }

    #[test]
    fn table_allocation_bounds_and_reference_types_are_checked() {
        let mut store = Store::new(limits());
        let index = store.allocate_table(RefType::FuncRef, 1, Some(2)).unwrap();
        store
            .table_mut(index)
            .unwrap()
            .set(0, RefValue::FuncRef(7))
            .unwrap();
        assert_eq!(
            store.table(index).unwrap().get(0).unwrap(),
            Some(RefValue::FuncRef(7))
        );
        assert_eq!(
            store.table_mut(index).unwrap().grow(2, 4),
            Err(StoreError::LimitExceeded("table limit exceeded"))
        );
        assert_eq!(
            store.table(index).unwrap().get(9),
            Err(StoreError::TableOutOfBounds)
        );
    }

    #[test]
    fn globals_enforce_type_and_mutability() {
        let mut store = Store::new(limits());
        let global_type = GlobalType {
            value_type: ValueType::I32,
            mutable: false,
        };
        assert!(store.add_global(global_type, Value::I64(1)).is_err());
        let index = store.add_global(global_type, Value::I32(1)).unwrap();
        assert_eq!(
            store.set_global(index, Value::I32(2)),
            Err(StoreError::ImmutableGlobal(index))
        );
        assert_eq!(store.global(index).unwrap().value, Value::I32(1));
    }
}

#[derive(Debug, Clone)]
pub struct MemoryInstance {
    pub data: Vec<u8>,
    pub min_pages: u64,
    pub max_pages: Option<u64>,
    /// The module-declared maximum, preserved separately from the effective
    /// resource-policy maximum used by growth checks.
    pub declared_max_pages: Option<u64>,
}

impl MemoryInstance {
    pub fn new(
        min_pages: u64,
        max_pages: Option<u64>,
        resource_limit: u64,
    ) -> Result<Self, StoreError> {
        let effective_max = max_pages.unwrap_or(resource_limit).min(resource_limit);
        if min_pages > effective_max {
            return Err(StoreError::InvalidLimits("memory minimum exceeds maximum"));
        }
        let byte_len = usize::try_from(
            min_pages
                .checked_mul(WASM_PAGE_SIZE as u64)
                .ok_or(StoreError::InvalidLimits("memory size overflows usize"))?,
        )
        .map_err(|_| StoreError::InvalidLimits("memory size overflows usize"))?;
        let mut data = Vec::new();
        data.try_reserve_exact(byte_len)
            .map_err(|_| StoreError::LimitExceeded("memory allocation exceeds limit"))?;
        data.resize(byte_len, 0);
        Ok(Self {
            data,
            min_pages,
            max_pages: Some(effective_max),
            declared_max_pages: max_pages,
        })
    }

    pub fn pages(&self) -> u64 {
        (self.data.len() as u64) / WASM_PAGE_SIZE as u64
    }

    pub fn grow(&mut self, delta_pages: u64, resource_limit: u64) -> Result<u64, StoreError> {
        let old_pages = self.pages();
        let new_pages = old_pages
            .checked_add(delta_pages)
            .ok_or(StoreError::LimitExceeded("memory page count overflows"))?;
        let maximum = self.max_pages.unwrap_or(resource_limit).min(resource_limit);
        if new_pages > maximum {
            return Err(StoreError::LimitExceeded("memory limit exceeded"));
        }
        let new_len = usize::try_from(
            new_pages
                .checked_mul(WASM_PAGE_SIZE as u64)
                .ok_or(StoreError::LimitExceeded("memory size overflows"))?,
        )
        .map_err(|_| StoreError::LimitExceeded("memory size overflows"))?;
        self.data
            .try_reserve_exact(new_len.saturating_sub(self.data.len()))
            .map_err(|_| StoreError::LimitExceeded("memory allocation exceeds limit"))?;
        self.data.resize(new_len, 0);
        Ok(old_pages)
    }

    pub fn read(&self, offset: u64, length: usize) -> Result<&[u8], StoreError> {
        let start = usize::try_from(offset).map_err(|_| StoreError::MemoryOutOfBounds)?;
        let end = start
            .checked_add(length)
            .ok_or(StoreError::MemoryOutOfBounds)?;
        self.data
            .get(start..end)
            .ok_or(StoreError::MemoryOutOfBounds)
    }

    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<(), StoreError> {
        let start = usize::try_from(offset).map_err(|_| StoreError::MemoryOutOfBounds)?;
        let end = start
            .checked_add(bytes.len())
            .ok_or(StoreError::MemoryOutOfBounds)?;
        let target = self
            .data
            .get_mut(start..end)
            .ok_or(StoreError::MemoryOutOfBounds)?;
        target.copy_from_slice(bytes);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TableInstance {
    pub element_type: RefType,
    pub elements: Vec<Option<RefValue>>,
    pub min_elements: u32,
    pub max_elements: Option<u32>,
    /// The module-declared maximum, preserved separately from the effective
    /// resource-policy maximum used by growth checks.
    pub declared_max_elements: Option<u32>,
}

impl TableInstance {
    pub fn new(
        element_type: RefType,
        min_elements: u32,
        max_elements: Option<u32>,
        resource_limit: u32,
    ) -> Result<Self, StoreError> {
        let effective_max = max_elements.unwrap_or(resource_limit).min(resource_limit);
        if min_elements > effective_max {
            return Err(StoreError::InvalidLimits("table minimum exceeds maximum"));
        }
        let mut elements = Vec::new();
        elements
            .try_reserve_exact(min_elements as usize)
            .map_err(|_| StoreError::LimitExceeded("table allocation exceeds limit"))?;
        elements.resize(min_elements as usize, None);
        Ok(Self {
            element_type,
            elements,
            min_elements,
            max_elements: Some(effective_max),
            declared_max_elements: max_elements,
        })
    }

    pub fn get(&self, index: u32) -> Result<Option<RefValue>, StoreError> {
        self.elements
            .get(index as usize)
            .cloned()
            .ok_or(StoreError::TableOutOfBounds)
    }

    pub fn set(&mut self, index: u32, value: RefValue) -> Result<(), StoreError> {
        let slot = self
            .elements
            .get_mut(index as usize)
            .ok_or(StoreError::TableOutOfBounds)?;
        let actual = match value {
            RefValue::Null(kind) => kind,
            RefValue::FuncRef(_) => RefType::FuncRef,
            RefValue::ExternRef(_) => RefType::ExternRef,
        };
        if actual != self.element_type {
            return Err(StoreError::InvalidReferenceType);
        }
        *slot = Some(value);
        Ok(())
    }

    pub fn grow(&mut self, delta: u32, resource_limit: u32) -> Result<u32, StoreError> {
        let old_len = self.elements.len();
        let new_len = old_len
            .checked_add(delta as usize)
            .ok_or(StoreError::LimitExceeded("table size overflows"))?;
        let maximum = self
            .max_elements
            .unwrap_or(resource_limit)
            .min(resource_limit) as usize;
        if new_len > maximum {
            return Err(StoreError::LimitExceeded("table limit exceeded"));
        }
        self.elements
            .try_reserve_exact(new_len.saturating_sub(old_len))
            .map_err(|_| StoreError::LimitExceeded("table allocation exceeds limit"))?;
        self.elements.resize(new_len, None);
        u32::try_from(old_len).map_err(|_| StoreError::LimitExceeded("table size exceeds u32"))
    }
}

#[derive(Debug, Clone)]
pub struct GlobalInstance {
    pub global_type: GlobalType,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub enum FuncInstance {
    Wasm(WasmFuncInstance),
    Host(HostFuncInstance),
}

#[derive(Debug, Clone)]
pub struct WasmFuncInstance {
    pub func_type: FunctionType,
    pub instance_idx: u32,
    pub func_idx: u32,
    pub instrs: Vec<crate::instr::Instr>,
    pub local_types: Vec<ValueType>,
}

#[derive(Debug, Clone)]
pub struct HostFuncInstance {
    pub func_type: FunctionType,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub module_types: Vec<FunctionType>,
    pub func_addrs: Vec<u32>,
    pub table_addrs: Vec<u32>,
    pub memory_addrs: Vec<u32>,
    pub global_addrs: Vec<u32>,
}
