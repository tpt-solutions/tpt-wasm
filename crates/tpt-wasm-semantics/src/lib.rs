// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Formal abstract machine for WebAssembly.
//!
//! This crate is an executable Rust model of the Core specification's
//! configuration and transition boundary. It deliberately does not depend on
//! the Micro interpreter or on compiler internals; correspondence is checked
//! separately by the verification layer.

use tpt_wasm_types::{FunctionType, GlobalType, RefType, RefValue, Trap, Value, ValueType};

const MAX_CALL_DEPTH: usize = 1000;

/// WebAssembly page size in bytes.
pub const PAGE_SIZE: usize = 65_536;

/// Errors raised while constructing or transitioning a model configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    StackUnderflow,
    FrameStackUnderflow,
    ControlStackUnderflow,
    UnknownFunction(u32),
    UnknownMemory(u32),
    UnknownTable(u32),
    UnknownGlobal(u32),
    UnknownLocal(u32),
    TypeMismatch {
        expected: ValueType,
        actual: ValueType,
    },
    PcOutOfBounds,
    MemoryOutOfBounds,
    InvalidLimit(&'static str),
    LimitExceeded(&'static str),
    InvariantViolation(&'static str),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ModelError {}

/// Abstract memory state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryState {
    pub bytes: Vec<u8>,
    pub max_pages: Option<u64>,
}

impl MemoryState {
    pub fn new(pages: u64, max_pages: Option<u64>) -> Result<Self, ModelError> {
        let maximum = max_pages.unwrap_or(u64::from(u16::MAX));
        if pages > maximum {
            return Err(ModelError::InvalidLimit("memory minimum exceeds maximum"));
        }
        let length = pages
            .checked_mul(PAGE_SIZE as u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(ModelError::LimitExceeded("memory size overflows usize"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| ModelError::LimitExceeded("memory allocation exceeds limit"))?;
        bytes.resize(length, 0);
        Ok(Self {
            bytes,
            max_pages: Some(maximum),
        })
    }

    pub fn pages(&self) -> u64 {
        (self.bytes.len() / PAGE_SIZE) as u64
    }
}

/// Abstract table state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableState {
    pub element_type: RefType,
    pub elements: Vec<Option<RefValue>>,
    pub max_elements: Option<u32>,
}

/// Abstract global state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalState {
    pub global_type: GlobalType,
    pub value: Value,
}

/// A function definition used by the model machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionState {
    pub function_type: FunctionType,
    pub local_types: Vec<ValueType>,
    pub body: Vec<Instruction>,
}

/// Store component of an abstract configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Store {
    pub memories: Vec<MemoryState>,
    pub tables: Vec<TableState>,
    pub globals: Vec<GlobalState>,
    pub functions: Vec<FunctionState>,
}

/// The subset of Core instructions modeled by the executable M5 machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    Unreachable,
    Nop,
    I32Const(i32),
    I64Const(i64),
    F32Const(u32),
    F64Const(u64),
    Drop,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    Call(u32),
    SelectI32,
    SelectI64,
    SelectF32,
    SelectF64,
    AddI32,
    SubI32,
    MulI32,
    DivSI32,
    DivUI32,
    RemSI32,
    RemUI32,
    AndI32,
    OrI32,
    XorI32,
    ShlI32,
    ShrSI32,
    ShrUI32,
    RotlI32,
    RotrI32,
    EqzI32,
    UnaryI32(UnaryOperation),
    CompareI32(Comparison),
    AddI64,
    SubI64,
    MulI64,
    DivSI64,
    DivUI64,
    RemSI64,
    RemUI64,
    AndI64,
    OrI64,
    XorI64,
    ShlI64,
    ShrSI64,
    ShrUI64,
    RotlI64,
    RotrI64,
    EqzI64,
    UnaryI64(UnaryOperation),
    CompareI64(Comparison),
    CompareF32(FloatComparison),
    CompareF64(FloatComparison),
    UnaryF32(FloatUnaryOperation),
    UnaryF64(FloatUnaryOperation),
    AddF32,
    SubF32,
    MulF32,
    DivF32,
    MinF32,
    MaxF32,
    CopysignF32,
    AddF64,
    SubF64,
    MulF64,
    DivF64,
    MinF64,
    MaxF64,
    CopysignF64,
    IntConvert(IntegerConversion),
    Reinterpret(ReinterpretOperation),
    FloatConvert(FloatConversion),
    FloatTrunc(FloatTrunc),
    Return,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerConversion {
    I32WrapI64,
    I64ExtendI32S,
    I64ExtendI32U,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReinterpretOperation {
    I32FromF32,
    I64FromF64,
    F32FromI32,
    F64FromI64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatConversion {
    F32FromI32S,
    F32FromI32U,
    F32FromI64S,
    F32FromI64U,
    F32FromF64,
    F64FromI32S,
    F64FromI32U,
    F64FromI64S,
    F64FromI64U,
    F64FromF32,
}

/// A trapping conversion from a floating-point value to an integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatTrunc {
    I32FromF32S,
    I32FromF32U,
    I32FromF64S,
    I32FromF64U,
    I64FromF32S,
    I64FromF32U,
    I64FromF64S,
    I64FromF64U,
}

fn trunc_f32_to_i32(value: f32, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if truncated < i32::MIN as f32 || truncated >= 2_147_483_648.0f32 {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I32(truncated as i32))
    } else if !(0.0..4_294_967_296.0f32).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I32(truncated as u32 as i32))
    }
}

fn trunc_f64_to_i32(value: f64, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if truncated < i32::MIN as f64 || truncated >= 2_147_483_648.0f64 {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I32(truncated as i32))
    } else if !(0.0..4_294_967_296.0f64).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I32(truncated as u32 as i32))
    }
}

fn trunc_f32_to_i64(value: f32, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if !(-9_223_372_036_854_775_808.0f32..9_223_372_036_854_775_808.0f32).contains(&truncated) {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I64(truncated as i64))
    } else if !(0.0..18_446_744_073_709_551_616.0f32).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I64(truncated as u64 as i64))
    }
}

fn trunc_f64_to_i64(value: f64, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if !(-9_223_372_036_854_775_808.0f64..9_223_372_036_854_775_808.0f64).contains(&truncated) {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I64(truncated as i64))
    } else if !(0.0..18_446_744_073_709_551_616.0f64).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I64(truncated as u64 as i64))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatUnaryOperation {
    Abs,
    Neg,
    Ceil,
    Floor,
    Trunc,
    Nearest,
    Sqrt,
}

fn unary_f32(value: f32, operation: FloatUnaryOperation) -> f32 {
    match operation {
        FloatUnaryOperation::Abs if value.is_nan() => f32::from_bits(value.to_bits() & 0x7fff_ffff),
        FloatUnaryOperation::Neg if value.is_nan() => f32::from_bits(value.to_bits() ^ 0x8000_0000),
        FloatUnaryOperation::Abs => value.abs(),
        FloatUnaryOperation::Neg => -value,
        FloatUnaryOperation::Ceil => value.ceil(),
        FloatUnaryOperation::Floor => value.floor(),
        FloatUnaryOperation::Trunc => value.trunc(),
        FloatUnaryOperation::Nearest => value.round_ties_even(),
        FloatUnaryOperation::Sqrt => value.sqrt(),
    }
}

fn unary_f64(value: f64, operation: FloatUnaryOperation) -> f64 {
    match operation {
        FloatUnaryOperation::Abs if value.is_nan() => {
            f64::from_bits(value.to_bits() & 0x7fff_ffff_ffff_ffff)
        }
        FloatUnaryOperation::Neg if value.is_nan() => {
            f64::from_bits(value.to_bits() ^ 0x8000_0000_0000_0000)
        }
        FloatUnaryOperation::Abs => value.abs(),
        FloatUnaryOperation::Neg => -value,
        FloatUnaryOperation::Ceil => value.ceil(),
        FloatUnaryOperation::Floor => value.floor(),
        FloatUnaryOperation::Trunc => value.trunc(),
        FloatUnaryOperation::Nearest => value.round_ties_even(),
        FloatUnaryOperation::Sqrt => value.sqrt(),
    }
}

fn float_min_f32(left: f32, right: f32) -> f32 {
    if left.is_nan() || right.is_nan() {
        f32::NAN
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else if left < right {
        left
    } else {
        right
    }
}

fn float_max_f32(left: f32, right: f32) -> f32 {
    if left.is_nan() || right.is_nan() {
        f32::NAN
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_positive() || right.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else if left > right {
        left
    } else {
        right
    }
}

fn float_min_f64(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else if left < right {
        left
    } else {
        right
    }
}

fn float_max_f64(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_positive() || right.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else if left > right {
        left
    } else {
        right
    }
}

const F32_CANONICAL_NAN: u32 = 0x7fc0_0000;
const F64_CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

fn f32_result(value: f32) -> u32 {
    if value.is_nan() {
        F32_CANONICAL_NAN
    } else {
        value.to_bits()
    }
}

fn f64_result(value: f64) -> u64 {
    if value.is_nan() {
        F64_CANONICAL_NAN
    } else {
        value.to_bits()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperation {
    Clz,
    Ctz,
    Popcnt,
}

fn unary_i32(value: i32, operation: UnaryOperation) -> i32 {
    match operation {
        UnaryOperation::Clz => value.leading_zeros() as i32,
        UnaryOperation::Ctz => value.trailing_zeros() as i32,
        UnaryOperation::Popcnt => value.count_ones() as i32,
    }
}

fn unary_i64(value: i64, operation: UnaryOperation) -> i64 {
    match operation {
        UnaryOperation::Clz => value.leading_zeros() as i64,
        UnaryOperation::Ctz => value.trailing_zeros() as i64,
        UnaryOperation::Popcnt => value.count_ones() as i64,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatComparison {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

fn compare_f32(left: f32, right: f32, comparison: FloatComparison) -> i32 {
    let result = match comparison {
        FloatComparison::Eq => left == right,
        FloatComparison::Ne => left != right,
        FloatComparison::Lt => left < right,
        FloatComparison::Gt => left > right,
        FloatComparison::Le => left <= right,
        FloatComparison::Ge => left >= right,
    };
    result as i32
}

fn compare_f64(left: f64, right: f64, comparison: FloatComparison) -> i32 {
    let result = match comparison {
        FloatComparison::Eq => left == right,
        FloatComparison::Ne => left != right,
        FloatComparison::Lt => left < right,
        FloatComparison::Gt => left > right,
        FloatComparison::Le => left <= right,
        FloatComparison::Ge => left >= right,
    };
    result as i32
}

fn local_type(function: &FunctionState, index: u32) -> Result<ValueType, ModelError> {
    let index = index as usize;
    function
        .function_type
        .params
        .0
        .get(index)
        .or_else(|| {
            function
                .local_types
                .get(index.checked_sub(function.function_type.params.0.len())?)
        })
        .copied()
        .ok_or(ModelError::UnknownLocal(index as u32))
}

fn compare_i32(left: i32, right: i32, comparison: Comparison) -> i32 {
    let result = match comparison {
        Comparison::Eq => left == right,
        Comparison::Ne => left != right,
        Comparison::LtS => left < right,
        Comparison::LtU => (left as u32) < (right as u32),
        Comparison::GtS => left > right,
        Comparison::GtU => (left as u32) > (right as u32),
        Comparison::LeS => left <= right,
        Comparison::LeU => (left as u32) <= (right as u32),
        Comparison::GeS => left >= right,
        Comparison::GeU => (left as u32) >= (right as u32),
    };
    result as i32
}

fn compare_i64(left: i64, right: i64, comparison: Comparison) -> i32 {
    let result = match comparison {
        Comparison::Eq => left == right,
        Comparison::Ne => left != right,
        Comparison::LtS => left < right,
        Comparison::LtU => (left as u64) < (right as u64),
        Comparison::GtS => left > right,
        Comparison::GtU => (left as u64) > (right as u64),
        Comparison::LeS => left <= right,
        Comparison::LeU => (left as u64) <= (right as u64),
        Comparison::GeS => left >= right,
        Comparison::GeU => (left as u64) >= (right as u64),
    };
    result as i32
}

/// A frame in the abstract frame stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub function_index: u32,
    pub locals: Vec<Value>,
    pub pc: usize,
    pub return_arity: usize,
    pub operand_base: usize,
    pub control_base: usize,
}

/// A control label in the abstract control stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Block,
    Loop,
    If,
    Function,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFrame {
    pub kind: ControlKind,
    pub stack_height: usize,
    pub result_types: Vec<ValueType>,
}

/// The abstract machine configuration: C = (Store, FrameStack, OperandStack, ControlStack).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Configuration {
    pub store: Store,
    pub frames: Vec<Frame>,
    pub operand_stack: Vec<Value>,
    pub control_stack: Vec<ControlFrame>,
}

/// The result of one model transition C → C′.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Step(Configuration),
    Return(Vec<Value>),
    Trap(Trap),
}

impl Configuration {
    pub fn check_invariants(&self) -> Result<(), ModelError> {
        if self.frames.is_empty() && !self.control_stack.is_empty() {
            return Err(ModelError::InvariantViolation(
                "control stack exists without an active frame",
            ));
        }
        let mut previous_operand_base = 0usize;
        let mut previous_control_base = 0usize;
        for frame in &self.frames {
            let function = self
                .store
                .functions
                .get(frame.function_index as usize)
                .ok_or(ModelError::InvariantViolation(
                    "frame references unknown function",
                ))?;
            if frame.pc >= function.body.len() {
                return Err(ModelError::InvariantViolation(
                    "active frame program counter is out of bounds",
                ));
            }
            let expected_locals = function
                .function_type
                .params
                .0
                .len()
                .checked_add(function.local_types.len())
                .ok_or(ModelError::InvariantViolation("local count overflows"))?;
            if frame.locals.len() != expected_locals {
                return Err(ModelError::InvariantViolation(
                    "frame local arity is invalid",
                ));
            }
            if frame.return_arity != function.function_type.results.0.len() {
                return Err(ModelError::InvariantViolation(
                    "frame return arity does not match its function",
                ));
            }
            if frame.operand_base < previous_operand_base
                || frame.operand_base > self.operand_stack.len()
            {
                return Err(ModelError::InvariantViolation("invalid operand base"));
            }
            if frame.control_base < previous_control_base
                || frame.control_base > self.control_stack.len()
            {
                return Err(ModelError::InvariantViolation("invalid control base"));
            }
            previous_operand_base = frame.operand_base;
            previous_control_base = frame.control_base;
        }
        for control in &self.control_stack {
            if control.stack_height > self.operand_stack.len() {
                return Err(ModelError::InvariantViolation(
                    "control frame stack height exceeds operand stack",
                ));
            }
        }
        for global in &self.store.globals {
            if Configuration::value_type(&global.value) != global.global_type.value_type {
                return Err(ModelError::InvariantViolation(
                    "global value does not match its declared type",
                ));
            }
        }
        Ok(())
    }

    fn default_value(value_type: ValueType) -> Value {
        match value_type {
            ValueType::I32 => Value::I32(0),
            ValueType::I64 => Value::I64(0),
            ValueType::F32 => Value::F32(0),
            ValueType::F64 => Value::F64(0),
            ValueType::V128 => Value::V128(0),
            ValueType::Ref(kind) => Value::Ref(RefValue::Null(kind)),
        }
    }

    pub fn value_type(value: &Value) -> ValueType {
        match value {
            Value::I32(_) => ValueType::I32,
            Value::I64(_) => ValueType::I64,
            Value::F32(_) => ValueType::F32,
            Value::F64(_) => ValueType::F64,
            Value::V128(_) => ValueType::V128,
            Value::Ref(RefValue::Null(kind)) => ValueType::Ref(*kind),
            Value::Ref(RefValue::FuncRef(_)) => ValueType::Ref(RefType::FuncRef),
            Value::Ref(RefValue::ExternRef(_)) => ValueType::Ref(RefType::ExternRef),
        }
    }

    pub fn push_value(&mut self, value: Value) -> Result<(), ModelError> {
        self.operand_stack.push(value);
        Ok(())
    }

    pub fn pop_value(&mut self) -> Result<Value, ModelError> {
        self.operand_stack.pop().ok_or(ModelError::StackUnderflow)
    }

    pub fn pop_typed(&mut self, expected: ValueType) -> Result<Value, ModelError> {
        let value = self.pop_value()?;
        let actual = Self::value_type(&value);
        if actual != expected {
            return Err(ModelError::TypeMismatch { expected, actual });
        }
        Ok(value)
    }

    pub fn pop_i32(&mut self) -> Result<i32, ModelError> {
        match self.pop_typed(ValueType::I32)? {
            Value::I32(value) => Ok(value),
            _ => unreachable!("typed pop preserves the value variant"),
        }
    }

    pub fn pop_i64(&mut self) -> Result<i64, ModelError> {
        match self.pop_typed(ValueType::I64)? {
            Value::I64(value) => Ok(value),
            _ => unreachable!("typed pop preserves the value variant"),
        }
    }

    pub fn pop_f32(&mut self) -> Result<f32, ModelError> {
        match self.pop_typed(ValueType::F32)? {
            Value::F32(value) => Ok(f32::from_bits(value)),
            _ => unreachable!("typed pop preserves the value variant"),
        }
    }

    pub fn pop_f64(&mut self) -> Result<f64, ModelError> {
        match self.pop_typed(ValueType::F64)? {
            Value::F64(value) => Ok(f64::from_bits(value)),
            _ => unreachable!("typed pop preserves the value variant"),
        }
    }

    pub fn push_frame(&mut self, frame: Frame) -> Result<(), ModelError> {
        if frame.function_index as usize >= self.store.functions.len() {
            return Err(ModelError::UnknownFunction(frame.function_index));
        }
        self.frames.push(frame);
        Ok(())
    }

    pub fn pop_frame(&mut self) -> Result<Frame, ModelError> {
        self.frames.pop().ok_or(ModelError::FrameStackUnderflow)
    }

    fn local_value(&self, index: u32) -> Result<&Value, ModelError> {
        let frame = self.frames.last().ok_or(ModelError::FrameStackUnderflow)?;
        frame
            .locals
            .get(index as usize)
            .ok_or(ModelError::UnknownLocal(index))
    }

    fn set_local(&mut self, index: u32, value: Value) -> Result<(), ModelError> {
        let frame = self
            .frames
            .last_mut()
            .ok_or(ModelError::FrameStackUnderflow)?;
        let local = frame
            .locals
            .get_mut(index as usize)
            .ok_or(ModelError::UnknownLocal(index))?;
        *local = value;
        Ok(())
    }

    pub fn push_control(&mut self, control: ControlFrame) {
        self.control_stack.push(control);
    }

    pub fn pop_control(&mut self) -> Result<ControlFrame, ModelError> {
        self.control_stack
            .pop()
            .ok_or(ModelError::ControlStackUnderflow)
    }

    pub fn step(&mut self) -> Result<Transition, ModelError> {
        let frame = self.frames.last().ok_or(ModelError::FrameStackUnderflow)?;
        let function_index = frame.function_index;
        let pc = frame.pc;
        let function = self
            .store
            .functions
            .get(function_index as usize)
            .ok_or(ModelError::UnknownFunction(function_index))?;
        let instruction = function
            .body
            .get(pc)
            .cloned()
            .ok_or(ModelError::PcOutOfBounds)?;
        self.frames.last_mut().expect("frame exists").pc += 1;
        match instruction {
            Instruction::Unreachable => return Ok(Transition::Trap(Trap::Unreachable)),
            Instruction::Nop => {}
            Instruction::I32Const(value) => self.push_value(Value::I32(value))?,
            Instruction::I64Const(value) => self.push_value(Value::I64(value))?,
            Instruction::F32Const(value) => self.push_value(Value::F32(value))?,
            Instruction::F64Const(value) => self.push_value(Value::F64(value))?,
            Instruction::Drop => {
                self.pop_value()?;
            }
            Instruction::LocalGet(index) => {
                let value = self.local_value(index)?.clone();
                self.push_value(value)?;
            }
            Instruction::LocalSet(index) => {
                let value_type = local_type(function, index)?;
                let value = self.pop_typed(value_type)?;
                self.set_local(index, value)?;
            }
            Instruction::LocalTee(index) => {
                let value_type = local_type(function, index)?;
                let value = self.pop_typed(value_type)?;
                self.set_local(index, value.clone())?;
                self.push_value(value)?;
            }
            Instruction::Call(callee_index) => {
                if self.frames.len() >= MAX_CALL_DEPTH {
                    return Ok(Transition::Trap(Trap::CallDepthExceeded));
                }
                let (parameters, local_types, result_arity) = {
                    let callee = self
                        .store
                        .functions
                        .get(callee_index as usize)
                        .ok_or(ModelError::UnknownFunction(callee_index))?;
                    (
                        callee.function_type.params.0.clone(),
                        callee.local_types.clone(),
                        callee.function_type.results.0.len(),
                    )
                };
                let mut arguments = Vec::with_capacity(parameters.len());
                for expected in parameters.iter().rev() {
                    arguments.push(self.pop_typed(*expected)?);
                }
                arguments.reverse();
                arguments.extend(local_types.into_iter().map(Self::default_value));
                self.frames.push(Frame {
                    function_index: callee_index,
                    locals: arguments,
                    pc: 0,
                    return_arity: result_arity,
                    operand_base: self.operand_stack.len(),
                    control_base: self.control_stack.len(),
                });
            }
            Instruction::SelectI32 => {
                let condition = self.pop_i32()?;
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(if condition != 0 { left } else { right }))?;
            }
            Instruction::SelectI64 => {
                let condition = self.pop_i32()?;
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(if condition != 0 { left } else { right }))?;
            }
            Instruction::SelectF32 => {
                let condition = self.pop_i32()?;
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32(
                    if condition != 0 { left } else { right }.to_bits(),
                ))?;
            }
            Instruction::SelectF64 => {
                let condition = self.pop_i32()?;
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64(
                    if condition != 0 { left } else { right }.to_bits(),
                ))?;
            }
            Instruction::AddI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.wrapping_add(right)))?;
            }
            Instruction::SubI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.wrapping_sub(right)))?;
            }
            Instruction::MulI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.wrapping_mul(right)))?;
            }
            Instruction::DivSI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                if left == i32::MIN && right == -1 {
                    return Ok(Transition::Trap(Trap::IntegerOverflow));
                }
                self.push_value(Value::I32(left / right))?;
            }
            Instruction::DivUI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                self.push_value(Value::I32(((left as u32) / (right as u32)) as i32))?;
            }
            Instruction::RemSI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                self.push_value(Value::I32(left.wrapping_rem(right)))?;
            }
            Instruction::RemUI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                self.push_value(Value::I32(((left as u32) % (right as u32)) as i32))?;
            }
            Instruction::AndI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left & right))?;
            }
            Instruction::OrI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left | right))?;
            }
            Instruction::XorI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left ^ right))?;
            }
            Instruction::ShlI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.wrapping_shl(right as u32)))?;
            }
            Instruction::ShrSI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.wrapping_shr(right as u32)))?;
            }
            Instruction::ShrUI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(
                    ((left as u32).wrapping_shr(right as u32)) as i32,
                ))?;
            }
            Instruction::RotlI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.rotate_left((right as u32) & 31)))?;
            }
            Instruction::RotrI32 => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(left.rotate_right((right as u32) & 31)))?;
            }
            Instruction::EqzI32 => {
                let value = self.pop_i32()?;
                self.push_value(Value::I32((value == 0) as i32))?;
            }
            Instruction::UnaryI32(operation) => {
                let value = self.pop_i32()?;
                self.push_value(Value::I32(unary_i32(value, operation)))?;
            }
            Instruction::CompareI32(comparison) => {
                let right = self.pop_i32()?;
                let left = self.pop_i32()?;
                self.push_value(Value::I32(compare_i32(left, right, comparison)))?;
            }
            Instruction::AddI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.wrapping_add(right)))?;
            }
            Instruction::SubI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.wrapping_sub(right)))?;
            }
            Instruction::MulI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.wrapping_mul(right)))?;
            }
            Instruction::DivSI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                if left == i64::MIN && right == -1 {
                    return Ok(Transition::Trap(Trap::IntegerOverflow));
                }
                self.push_value(Value::I64(left / right))?;
            }
            Instruction::DivUI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                self.push_value(Value::I64(((left as u64) / (right as u64)) as i64))?;
            }
            Instruction::RemSI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                self.push_value(Value::I64(left.wrapping_rem(right)))?;
            }
            Instruction::RemUI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                if right == 0 {
                    return Ok(Transition::Trap(Trap::IntegerDivisionByZero));
                }
                self.push_value(Value::I64(((left as u64) % (right as u64)) as i64))?;
            }
            Instruction::AndI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left & right))?;
            }
            Instruction::OrI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left | right))?;
            }
            Instruction::XorI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left ^ right))?;
            }
            Instruction::ShlI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.wrapping_shl(right as u32)))?;
            }
            Instruction::ShrSI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.wrapping_shr(right as u32)))?;
            }
            Instruction::ShrUI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(
                    ((left as u64).wrapping_shr(right as u32)) as i64,
                ))?;
            }
            Instruction::RotlI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.rotate_left((right as u64 & 63) as u32)))?;
            }
            Instruction::RotrI64 => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I64(left.rotate_right((right as u64 & 63) as u32)))?;
            }
            Instruction::EqzI64 => {
                let value = self.pop_i64()?;
                self.push_value(Value::I32((value == 0) as i32))?;
            }
            Instruction::UnaryI64(operation) => {
                let value = self.pop_i64()?;
                self.push_value(Value::I64(unary_i64(value, operation)))?;
            }
            Instruction::CompareI64(comparison) => {
                let right = self.pop_i64()?;
                let left = self.pop_i64()?;
                self.push_value(Value::I32(compare_i64(left, right, comparison)))?;
            }
            Instruction::CompareF32(comparison) => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::I32(compare_f32(left, right, comparison)))?;
            }
            Instruction::CompareF64(comparison) => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::I32(compare_f64(left, right, comparison)))?;
            }
            Instruction::UnaryF32(operation) => {
                let value = self.pop_f32()?;
                self.push_value(Value::F32(unary_f32(value, operation).to_bits()))?;
            }
            Instruction::UnaryF64(operation) => {
                let value = self.pop_f64()?;
                self.push_value(Value::F64(unary_f64(value, operation).to_bits()))?;
            }
            Instruction::AddF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32((left + right).to_bits()))?;
            }
            Instruction::SubF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32((left - right).to_bits()))?;
            }
            Instruction::MulF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32((left * right).to_bits()))?;
            }
            Instruction::DivF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32((left / right).to_bits()))?;
            }
            Instruction::MinF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32(float_min_f32(left, right).to_bits()))?;
            }
            Instruction::MaxF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32(float_max_f32(left, right).to_bits()))?;
            }
            Instruction::CopysignF32 => {
                let right = self.pop_f32()?;
                let left = self.pop_f32()?;
                self.push_value(Value::F32(left.copysign(right).to_bits()))?;
            }
            Instruction::AddF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64((left + right).to_bits()))?;
            }
            Instruction::SubF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64((left - right).to_bits()))?;
            }
            Instruction::MulF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64((left * right).to_bits()))?;
            }
            Instruction::DivF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64((left / right).to_bits()))?;
            }
            Instruction::MinF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64(float_min_f64(left, right).to_bits()))?;
            }
            Instruction::MaxF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64(float_max_f64(left, right).to_bits()))?;
            }
            Instruction::CopysignF64 => {
                let right = self.pop_f64()?;
                let left = self.pop_f64()?;
                self.push_value(Value::F64(left.copysign(right).to_bits()))?;
            }
            Instruction::IntConvert(operation) => match operation {
                IntegerConversion::I32WrapI64 => {
                    let value = self.pop_i64()?;
                    self.push_value(Value::I32(value as i32))?;
                }
                IntegerConversion::I64ExtendI32S => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::I64(i64::from(value)))?;
                }
                IntegerConversion::I64ExtendI32U => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::I64(i64::from(value as u32)))?;
                }
            },
            Instruction::Reinterpret(operation) => match operation {
                ReinterpretOperation::I32FromF32 => {
                    let value = self.pop_typed(ValueType::F32)?;
                    let Value::F32(bits) = value else {
                        unreachable!("typed pop preserves the value variant")
                    };
                    self.push_value(Value::I32(bits as i32))?;
                }
                ReinterpretOperation::I64FromF64 => {
                    let value = self.pop_typed(ValueType::F64)?;
                    let Value::F64(bits) = value else {
                        unreachable!("typed pop preserves the value variant")
                    };
                    self.push_value(Value::I64(bits as i64))?;
                }
                ReinterpretOperation::F32FromI32 => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::F32(value as u32))?;
                }
                ReinterpretOperation::F64FromI64 => {
                    let value = self.pop_i64()?;
                    self.push_value(Value::F64(value as u64))?;
                }
            },
            Instruction::FloatConvert(operation) => match operation {
                FloatConversion::F32FromI32S => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::F32(f32_result(value as f32)))?;
                }
                FloatConversion::F32FromI32U => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::F32(f32_result(value as u32 as f32)))?;
                }
                FloatConversion::F32FromI64S => {
                    let value = self.pop_i64()?;
                    self.push_value(Value::F32(f32_result(value as f32)))?;
                }
                FloatConversion::F32FromI64U => {
                    let value = self.pop_i64()?;
                    self.push_value(Value::F32(f32_result(value as u64 as f32)))?;
                }
                FloatConversion::F32FromF64 => {
                    let value = self.pop_f64()?;
                    self.push_value(Value::F32(f32_result(value as f32)))?;
                }
                FloatConversion::F64FromI32S => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::F64(f64_result(value as f64)))?;
                }
                FloatConversion::F64FromI32U => {
                    let value = self.pop_i32()?;
                    self.push_value(Value::F64(f64_result(value as u32 as f64)))?;
                }
                FloatConversion::F64FromI64S => {
                    let value = self.pop_i64()?;
                    self.push_value(Value::F64(f64_result(value as f64)))?;
                }
                FloatConversion::F64FromI64U => {
                    let value = self.pop_i64()?;
                    self.push_value(Value::F64(f64_result(value as u64 as f64)))?;
                }
                FloatConversion::F64FromF32 => {
                    let value = self.pop_f32()?;
                    self.push_value(Value::F64(f64_result(value as f64)))?;
                }
            },
            Instruction::FloatTrunc(operation) => match operation {
                FloatTrunc::I32FromF32S => match trunc_f32_to_i32(self.pop_f32()?, true) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I32FromF32U => match trunc_f32_to_i32(self.pop_f32()?, false) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I32FromF64S => match trunc_f64_to_i32(self.pop_f64()?, true) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I32FromF64U => match trunc_f64_to_i32(self.pop_f64()?, false) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I64FromF32S => match trunc_f32_to_i64(self.pop_f32()?, true) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I64FromF32U => match trunc_f32_to_i64(self.pop_f32()?, false) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I64FromF64S => match trunc_f64_to_i64(self.pop_f64()?, true) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
                FloatTrunc::I64FromF64U => match trunc_f64_to_i64(self.pop_f64()?, false) {
                    Ok(value) => self.push_value(value)?,
                    Err(trap) => return Ok(Transition::Trap(trap)),
                },
            },
            Instruction::Return | Instruction::End => {
                let frame = self.pop_frame()?;
                let expected_len = frame
                    .operand_base
                    .checked_add(frame.return_arity)
                    .ok_or(ModelError::StackUnderflow)?;
                if self.operand_stack.len() != expected_len {
                    return Err(ModelError::StackUnderflow);
                }
                let results = self.operand_stack.split_off(frame.operand_base);
                self.control_stack.truncate(frame.control_base);
                return if self.frames.is_empty() {
                    Ok(Transition::Return(results))
                } else {
                    self.operand_stack.extend(results);
                    Ok(Transition::Step(self.clone()))
                };
            }
        };
        Ok(Transition::Step(self.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Comparison, Configuration, ControlFrame, ControlKind, FloatComparison, FloatConversion,
        FloatTrunc, FloatUnaryOperation, Frame, FunctionState, Instruction, IntegerConversion,
        ModelError, ReinterpretOperation, Store, Transition, Trap, UnaryOperation,
    };
    use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

    fn configuration_from_function(
        results: Vec<ValueType>,
        body: Vec<Instruction>,
        return_arity: usize,
    ) -> Configuration {
        let mut configuration = Configuration {
            store: Store {
                functions: vec![FunctionState {
                    function_type: FunctionType {
                        params: ResultType(Vec::new()),
                        results: ResultType(results),
                    },
                    local_types: Vec::new(),
                    body,
                }],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 0,
                locals: Vec::new(),
                pc: 0,
                return_arity,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        configuration
    }

    fn function() -> FunctionState {
        FunctionState {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            local_types: Vec::new(),
            body: vec![
                Instruction::I32Const(20),
                Instruction::I32Const(22),
                Instruction::AddI32,
                Instruction::End,
            ],
        }
    }

    fn configuration() -> Configuration {
        let mut configuration = Configuration {
            store: Store {
                functions: vec![function()],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 0,
                locals: Vec::new(),
                pc: 0,
                return_arity: 1,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        configuration
    }

    #[test]
    fn transitions_preserve_configuration_and_return_values() {
        let mut current = configuration();
        current = match current.step().unwrap() {
            Transition::Step(next) => next,
            other => panic!("unexpected transition: {other:?}"),
        };
        current = match current.step().unwrap() {
            Transition::Step(next) => next,
            other => panic!("unexpected transition: {other:?}"),
        };
        current = match current.step().unwrap() {
            Transition::Step(next) => next,
            other => panic!("unexpected transition: {other:?}"),
        };
        assert_eq!(
            current.step().unwrap(),
            Transition::Return(vec![Value::I32(42)])
        );
    }

    #[test]
    fn integer_multiplication_wraps_in_the_model() {
        let mut configuration = configuration();
        configuration.store.functions[0].body = vec![
            Instruction::I32Const(6),
            Instruction::I32Const(7),
            Instruction::MulI32,
            Instruction::End,
        ];
        for _ in 0..3 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(42)])
        );
    }

    #[test]
    fn float_arithmetic_preserves_ieee754_edge_results() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::F32, ValueType::F32, ValueType::F64]);
        configuration.frames[0].return_arity = 3;
        configuration.store.functions[0].body = vec![
            Instruction::F32Const(1.0f32.to_bits()),
            Instruction::F32Const(0.0f32.to_bits()),
            Instruction::DivF32,
            Instruction::F32Const((-1.0f32).to_bits()),
            Instruction::F32Const(0.0f32.to_bits()),
            Instruction::DivF32,
            Instruction::F64Const(0.0f64.to_bits()),
            Instruction::F64Const(0.0f64.to_bits()),
            Instruction::DivF64,
            Instruction::End,
        ];
        for _ in 0..9 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        let Transition::Return(values) = configuration.step().unwrap() else {
            panic!("expected final return");
        };
        assert!(
            f32::from_bits(match values[0] {
                Value::F32(value) => value,
                _ => panic!("expected f32 result"),
            })
            .is_infinite()
                && f32::from_bits(match values[0] {
                    Value::F32(value) => value,
                    _ => unreachable!(),
                })
                .is_sign_positive()
        );
        assert!(
            f32::from_bits(match values[1] {
                Value::F32(value) => value,
                _ => panic!("expected f32 result"),
            })
            .is_infinite()
                && f32::from_bits(match values[1] {
                    Value::F32(value) => value,
                    _ => unreachable!(),
                })
                .is_sign_negative()
        );
        assert!(f64::from_bits(match values[2] {
            Value::F64(value) => value,
            _ => panic!("expected f64 result"),
        })
        .is_nan());
    }

    #[test]
    fn float_truncation_executes_and_traps_in_the_model() {
        let mut configuration = configuration_from_function(
            vec![
                ValueType::I32,
                ValueType::I32,
                ValueType::I32,
                ValueType::I32,
                ValueType::I64,
                ValueType::I64,
                ValueType::I64,
                ValueType::I64,
            ],
            vec![
                Instruction::F32Const(3.9f32.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I32FromF32S),
                Instruction::F32Const(3.9f32.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I32FromF32U),
                Instruction::F64Const(3.9f64.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I32FromF64S),
                Instruction::F64Const(3.9f64.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I32FromF64U),
                Instruction::F32Const(3.9f32.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I64FromF32S),
                Instruction::F32Const(3.9f32.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I64FromF32U),
                Instruction::F64Const(3.9f64.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I64FromF64S),
                Instruction::F64Const(3.9f64.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I64FromF64U),
                Instruction::End,
            ],
            8,
        );
        for _ in 0..16 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        let Transition::Return(values) = configuration.step().unwrap() else {
            panic!("expected final return");
        };
        assert_eq!(
            values,
            vec![
                Value::I32(3),
                Value::I32(3),
                Value::I32(3),
                Value::I32(3),
                Value::I64(3),
                Value::I64(3),
                Value::I64(3),
                Value::I64(3),
            ]
        );

        let mut trap = configuration_from_function(
            vec![ValueType::I32],
            vec![
                Instruction::F32Const(f32::NAN.to_bits()),
                Instruction::FloatTrunc(FloatTrunc::I32FromF32S),
                Instruction::End,
            ],
            1,
        );
        trap = match trap.step().unwrap() {
            Transition::Step(next) => next,
            other => panic!("unexpected transition: {other:?}"),
        };
        assert_eq!(
            trap.step().unwrap(),
            Transition::Trap(Trap::InvalidConversion)
        );
    }

    #[test]
    fn integer_division_and_remainder_follow_wasm_rules() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::I32, ValueType::I32, ValueType::I32]);
        configuration.frames[0].return_arity = 3;
        configuration.store.functions[0].body = vec![
            Instruction::I32Const(-42),
            Instruction::I32Const(6),
            Instruction::DivSI32,
            Instruction::I32Const(-1),
            Instruction::I32Const(2),
            Instruction::DivUI32,
            Instruction::I32Const(i32::MIN),
            Instruction::I32Const(-1),
            Instruction::RemSI32,
            Instruction::End,
        ];
        for _ in 0..9 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(-7),
                Value::I32(2_147_483_647),
                Value::I32(0)
            ])
        );

        let mut overflow = configuration_from_function(
            vec![ValueType::I32],
            vec![
                Instruction::I32Const(i32::MIN),
                Instruction::I32Const(-1),
                Instruction::DivSI32,
                Instruction::End,
            ],
            1,
        );
        for _ in 0..2 {
            overflow = match overflow.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            overflow.step().unwrap(),
            Transition::Trap(Trap::IntegerOverflow)
        );

        let mut zero = configuration_from_function(
            vec![ValueType::I64],
            vec![
                Instruction::I64Const(1),
                Instruction::I64Const(0),
                Instruction::DivUI64,
                Instruction::End,
            ],
            1,
        );
        for _ in 0..2 {
            zero = match zero.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            zero.step().unwrap(),
            Transition::Trap(Trap::IntegerDivisionByZero)
        );
    }

    #[test]
    fn i32_unary_operations_execute_in_the_model() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::I32, ValueType::I32, ValueType::I32]);
        configuration.frames[0].return_arity = 3;
        configuration.store.functions[0].body = vec![
            Instruction::I32Const(1),
            Instruction::UnaryI32(UnaryOperation::Clz),
            Instruction::I32Const(1),
            Instruction::UnaryI32(UnaryOperation::Ctz),
            Instruction::I32Const(1),
            Instruction::UnaryI32(UnaryOperation::Popcnt),
            Instruction::End,
        ];
        for _ in 0..6 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(31), Value::I32(0), Value::I32(1)])
        );
    }

    #[test]
    fn i32_bitwise_operations_execute_in_the_model() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::I32, ValueType::I32, ValueType::I32]);
        configuration.frames[0].return_arity = 3;
        configuration.store.functions[0].body = vec![
            Instruction::I32Const(0b1010),
            Instruction::I32Const(0b0110),
            Instruction::AndI32,
            Instruction::I32Const(0b1010),
            Instruction::I32Const(0b0110),
            Instruction::OrI32,
            Instruction::I32Const(0b1010),
            Instruction::I32Const(0b0110),
            Instruction::XorI32,
            Instruction::End,
        ];
        for _ in 0..9 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(0b0010),
                Value::I32(0b1110),
                Value::I32(0b1100),
            ])
        );
    }

    #[test]
    fn float_unary_operations_execute_in_the_model() {
        let operations = [
            FloatUnaryOperation::Abs,
            FloatUnaryOperation::Neg,
            FloatUnaryOperation::Ceil,
            FloatUnaryOperation::Floor,
            FloatUnaryOperation::Trunc,
            FloatUnaryOperation::Nearest,
            FloatUnaryOperation::Sqrt,
        ];
        let mut body = Vec::new();
        for operation in operations {
            body.extend([Instruction::F32Const(2.5f32.to_bits())]);
            body.push(Instruction::UnaryF32(operation));
        }
        for operation in operations {
            body.push(Instruction::F64Const(2.5f64.to_bits()));
            body.push(Instruction::UnaryF64(operation));
        }
        body.push(Instruction::End);
        let mut configuration = configuration_from_function(
            (0..7)
                .map(|_| ValueType::F32)
                .chain((0..7).map(|_| ValueType::F64))
                .collect(),
            body,
            14,
        );
        for _ in 0..28 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        let Transition::Return(values) = configuration.step().unwrap() else {
            panic!("expected final return");
        };
        assert_eq!(values.len(), 14);
        assert_eq!(values[0], Value::F32(2.5f32.to_bits()));
        assert_eq!(values[1], Value::F32((-2.5f32).to_bits()));
        assert_eq!(values[2], Value::F32(3.0f32.to_bits()));
        assert_eq!(values[6], Value::F32(2.5f32.sqrt().to_bits()));
        assert_eq!(values[7], Value::F64(2.5f64.to_bits()));
        assert_eq!(values[13], Value::F64(2.5f64.sqrt().to_bits()));
    }

    #[test]
    fn integer_conversions_execute_in_the_model() {
        let mut configuration = configuration_from_function(
            vec![ValueType::I32, ValueType::I64, ValueType::I64],
            vec![
                Instruction::I64Const(0x0000_0001_0000_002a),
                Instruction::IntConvert(IntegerConversion::I32WrapI64),
                Instruction::I32Const(-2),
                Instruction::IntConvert(IntegerConversion::I64ExtendI32S),
                Instruction::I32Const(-1),
                Instruction::IntConvert(IntegerConversion::I64ExtendI32U),
                Instruction::End,
            ],
            3,
        );
        for _ in 0..6 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(42),
                Value::I64(-2),
                Value::I64(4_294_967_295),
            ])
        );
    }

    #[test]
    fn float_conversions_execute_in_the_model() {
        let mut configuration = configuration_from_function(
            vec![
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
            ],
            vec![
                Instruction::I32Const(-1),
                Instruction::FloatConvert(FloatConversion::F32FromI32S),
                Instruction::I32Const(-1),
                Instruction::FloatConvert(FloatConversion::F32FromI32U),
                Instruction::I64Const(-1),
                Instruction::FloatConvert(FloatConversion::F32FromI64S),
                Instruction::I64Const(-1),
                Instruction::FloatConvert(FloatConversion::F32FromI64U),
                Instruction::F64Const(f64::NAN.to_bits()),
                Instruction::FloatConvert(FloatConversion::F32FromF64),
                Instruction::I32Const(-1),
                Instruction::FloatConvert(FloatConversion::F64FromI32S),
                Instruction::I32Const(-1),
                Instruction::FloatConvert(FloatConversion::F64FromI32U),
                Instruction::I64Const(-1),
                Instruction::FloatConvert(FloatConversion::F64FromI64S),
                Instruction::I64Const(-1),
                Instruction::FloatConvert(FloatConversion::F64FromI64U),
                Instruction::F32Const(f32::NAN.to_bits()),
                Instruction::FloatConvert(FloatConversion::F64FromF32),
                Instruction::End,
            ],
            10,
        );
        for _ in 0..20 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        let Transition::Return(values) = configuration.step().unwrap() else {
            panic!("expected final return");
        };
        assert_eq!(
            values,
            vec![
                Value::F32((-1.0f32).to_bits()),
                Value::F32((u32::MAX as f32).to_bits()),
                Value::F32((-1.0f32).to_bits()),
                Value::F32((u64::MAX as f32).to_bits()),
                Value::F32(0x7fc0_0000),
                Value::F64((-1.0f64).to_bits()),
                Value::F64((u32::MAX as f64).to_bits()),
                Value::F64((-1.0f64).to_bits()),
                Value::F64((u64::MAX as f64).to_bits()),
                Value::F64(0x7ff8_0000_0000_0000),
            ]
        );
    }

    #[test]
    fn reinterpret_operations_preserve_exact_bits_in_the_model() {
        let f32_bits = 0x7f80_1234u32;
        let f64_bits = 0x7ff0_1234_5678_9abcu64;
        let mut configuration = configuration_from_function(
            vec![
                ValueType::I32,
                ValueType::I64,
                ValueType::F32,
                ValueType::F64,
            ],
            vec![
                Instruction::F32Const(f32_bits),
                Instruction::Reinterpret(ReinterpretOperation::I32FromF32),
                Instruction::F64Const(f64_bits),
                Instruction::Reinterpret(ReinterpretOperation::I64FromF64),
                Instruction::I32Const(f32_bits as i32),
                Instruction::Reinterpret(ReinterpretOperation::F32FromI32),
                Instruction::I64Const(f64_bits as i64),
                Instruction::Reinterpret(ReinterpretOperation::F64FromI64),
                Instruction::End,
            ],
            4,
        );
        for _ in 0..8 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(f32_bits as i32),
                Value::I64(f64_bits as i64),
                Value::F32(f32_bits),
                Value::F64(f64_bits),
            ])
        );
    }

    #[test]
    fn select_operations_preserve_the_selected_typed_value() {
        let mut body = Vec::new();
        body.extend([
            Instruction::I32Const(10),
            Instruction::I32Const(20),
            Instruction::I32Const(1),
            Instruction::SelectI32,
            Instruction::I64Const(30),
            Instruction::I64Const(40),
            Instruction::I32Const(0),
            Instruction::SelectI64,
            Instruction::F32Const(1.5f32.to_bits()),
            Instruction::F32Const(2.5f32.to_bits()),
            Instruction::I32Const(1),
            Instruction::SelectF32,
            Instruction::F64Const(3.5f64.to_bits()),
            Instruction::F64Const(4.5f64.to_bits()),
            Instruction::I32Const(0),
            Instruction::SelectF64,
            Instruction::End,
        ]);
        let mut configuration = configuration_from_function(
            vec![
                ValueType::I32,
                ValueType::I64,
                ValueType::F32,
                ValueType::F64,
            ],
            body,
            4,
        );
        for _ in 0..16 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(10),
                Value::I64(40),
                Value::F32(1.5f32.to_bits()),
                Value::F64(4.5f64.to_bits()),
            ])
        );
    }

    #[test]
    fn float_binary_operations_execute_in_the_model() {
        let mut body = Vec::new();
        for instruction in [
            Instruction::MinF32,
            Instruction::MaxF32,
            Instruction::CopysignF32,
        ] {
            body.extend([Instruction::F32Const(2.5f32.to_bits())]);
            body.extend([Instruction::F32Const((-3.5f32).to_bits())]);
            body.push(instruction);
        }
        for instruction in [
            Instruction::MinF64,
            Instruction::MaxF64,
            Instruction::CopysignF64,
        ] {
            body.push(Instruction::F64Const(2.5f64.to_bits()));
            body.push(Instruction::F64Const((-3.5f64).to_bits()));
            body.push(instruction);
        }
        body.push(Instruction::End);
        let mut configuration = configuration_from_function(
            (0..3)
                .map(|_| ValueType::F32)
                .chain((0..3).map(|_| ValueType::F64))
                .collect(),
            body,
            6,
        );
        for _ in 0..18 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::F32((-3.5f32).to_bits()),
                Value::F32(2.5f32.to_bits()),
                Value::F32((-2.5f32).to_bits()),
                Value::F64((-3.5f64).to_bits()),
                Value::F64(2.5f64.to_bits()),
                Value::F64((-2.5f64).to_bits()),
            ])
        );
    }

    #[test]
    fn float_comparisons_follow_unordered_ieee754_rules() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::I32, ValueType::I32]);
        configuration.frames[0].return_arity = 2;
        configuration.store.functions[0].body = vec![
            Instruction::F32Const(1.0f32.to_bits()),
            Instruction::F32Const(2.0f32.to_bits()),
            Instruction::CompareF32(FloatComparison::Lt),
            Instruction::F32Const(f32::NAN.to_bits()),
            Instruction::F32Const(1.0f32.to_bits()),
            Instruction::CompareF32(FloatComparison::Ne),
            Instruction::End,
        ];
        for _ in 0..6 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(1), Value::I32(1)])
        );
    }

    #[test]
    fn integer_rotations_mask_shift_counts_to_operand_width() {
        let mut configuration = configuration_from_function(
            vec![
                ValueType::I32,
                ValueType::I32,
                ValueType::I64,
                ValueType::I64,
            ],
            vec![
                Instruction::I32Const(1),
                Instruction::I32Const(33),
                Instruction::RotlI32,
                Instruction::I32Const(i32::MIN),
                Instruction::I32Const(33),
                Instruction::RotrI32,
                Instruction::I64Const(1),
                Instruction::I64Const(65),
                Instruction::RotlI64,
                Instruction::I64Const(i64::MIN),
                Instruction::I64Const(65),
                Instruction::RotrI64,
                Instruction::End,
            ],
            4,
        );
        for _ in 0..12 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(2),
                Value::I32(1 << 30),
                Value::I64(2),
                Value::I64(1 << 62),
            ])
        );
    }

    #[test]
    fn i32_shift_operations_follow_wasm_masking_rules() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::I32, ValueType::I32, ValueType::I32]);
        configuration.frames[0].return_arity = 3;
        configuration.store.functions[0].body = vec![
            Instruction::I32Const(16),
            Instruction::I32Const(2),
            Instruction::ShlI32,
            Instruction::I32Const(i32::MIN),
            Instruction::I32Const(2),
            Instruction::ShrSI32,
            Instruction::I32Const(i32::MIN),
            Instruction::I32Const(2),
            Instruction::ShrUI32,
            Instruction::End,
        ];
        for _ in 0..9 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(64),
                Value::I32(-536_870_912),
                Value::I32(536_870_912)
            ])
        );
    }

    #[test]
    fn i64_integer_operations_execute_in_the_model() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results = ResultType(vec![
            ValueType::I64,
            ValueType::I64,
            ValueType::I64,
            ValueType::I64,
            ValueType::I64,
        ]);
        configuration.frames[0].return_arity = 5;
        configuration.store.functions[0].body = vec![
            Instruction::I64Const(50),
            Instruction::I64Const(8),
            Instruction::AddI64,
            Instruction::I64Const(6),
            Instruction::I64Const(7),
            Instruction::MulI64,
            Instruction::I64Const(0b1010),
            Instruction::I64Const(0b0110),
            Instruction::AndI64,
            Instruction::I64Const(1),
            Instruction::UnaryI64(UnaryOperation::Popcnt),
            Instruction::I64Const(-1),
            Instruction::I64Const(0),
            Instruction::CompareI64(Comparison::LtU),
            Instruction::End,
        ];
        for _ in 0..14 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I64(58),
                Value::I64(42),
                Value::I64(0b0010),
                Value::I64(1),
                Value::I32(0),
            ])
        );
    }

    #[test]
    fn i64_shift_operations_follow_wasm_masking_rules() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results =
            ResultType(vec![ValueType::I64, ValueType::I64, ValueType::I64]);
        configuration.frames[0].return_arity = 3;
        configuration.store.functions[0].body = vec![
            Instruction::I64Const(16),
            Instruction::I64Const(2),
            Instruction::ShlI64,
            Instruction::I64Const(i64::MIN),
            Instruction::I64Const(2),
            Instruction::ShrSI64,
            Instruction::I64Const(i64::MIN),
            Instruction::I64Const(2),
            Instruction::ShrUI64,
            Instruction::End,
        ];
        for _ in 0..9 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I64(64),
                Value::I64(-2_305_843_009_213_693_952),
                Value::I64(2_305_843_009_213_693_952),
            ])
        );
    }

    #[test]
    fn i64_unary_and_eqz_operations_execute_in_the_model() {
        let mut configuration = configuration();
        configuration.store.functions[0].function_type.results = ResultType(vec![
            ValueType::I64,
            ValueType::I64,
            ValueType::I64,
            ValueType::I32,
        ]);
        configuration.frames[0].return_arity = 4;
        configuration.store.functions[0].body = vec![
            Instruction::I64Const(1),
            Instruction::UnaryI64(UnaryOperation::Clz),
            Instruction::I64Const(1),
            Instruction::UnaryI64(UnaryOperation::Ctz),
            Instruction::I64Const(1),
            Instruction::UnaryI64(UnaryOperation::Popcnt),
            Instruction::I64Const(0),
            Instruction::EqzI64,
            Instruction::End,
        ];
        for _ in 0..8 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I64(63),
                Value::I64(0),
                Value::I64(1),
                Value::I32(1),
            ])
        );
    }

    #[test]
    fn typed_underflow_and_empty_stacks_are_errors() {
        let mut underflow = configuration();
        underflow.frames[0].pc = 2;
        underflow.frames[0].return_arity = 0;
        assert_eq!(underflow.step(), Err(ModelError::StackUnderflow));
        let mut no_frame = Configuration::default();
        assert_eq!(no_frame.step(), Err(ModelError::FrameStackUnderflow));
        assert_eq!(
            no_frame.pop_control(),
            Err(ModelError::ControlStackUnderflow)
        );
    }

    #[test]
    fn v0_invariants_accept_valid_and_reject_invalid_configurations() {
        let mut valid = configuration();
        assert_eq!(valid.check_invariants(), Ok(()));
        valid.frames[0].pc = valid.store.functions[0].body.len();
        assert_eq!(
            valid.check_invariants(),
            Err(ModelError::InvariantViolation(
                "active frame program counter is out of bounds"
            ))
        );
    }

    #[test]
    fn control_frames_are_nested_and_ordered() {
        let mut configuration = configuration();
        configuration.push_control(ControlFrame {
            kind: ControlKind::Function,
            stack_height: 0,
            result_types: vec![ValueType::I32],
        });
        configuration.push_control(ControlFrame {
            kind: ControlKind::Block,
            stack_height: 0,
            result_types: Vec::new(),
        });
        assert_eq!(
            configuration.pop_control().unwrap().kind,
            ControlKind::Block
        );
        assert_eq!(
            configuration.pop_control().unwrap().kind,
            ControlKind::Function
        );
    }

    #[test]
    fn direct_calls_preserve_caller_operand_base() {
        let callee = FunctionState {
            function_type: FunctionType {
                params: ResultType(vec![ValueType::I32, ValueType::I32]),
                results: ResultType(vec![ValueType::I32]),
            },
            local_types: Vec::new(),
            body: vec![
                Instruction::LocalGet(0),
                Instruction::LocalGet(1),
                Instruction::SubI32,
                Instruction::End,
            ],
        };
        let caller = FunctionState {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            local_types: Vec::new(),
            body: vec![
                Instruction::I32Const(100),
                Instruction::I32Const(20),
                Instruction::I32Const(21),
                Instruction::Call(0),
                Instruction::AddI32,
                Instruction::End,
            ],
        };
        let mut configuration = Configuration {
            store: Store {
                functions: vec![callee, caller],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 1,
                locals: Vec::new(),
                pc: 0,
                return_arity: 1,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        for _ in 0..9 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
            configuration.check_invariants().unwrap();
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(99)])
        );
    }
}
