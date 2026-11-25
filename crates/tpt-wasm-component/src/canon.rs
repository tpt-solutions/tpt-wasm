// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) TPT Solutions

//! The Canonical ABI for component values.
//!
//! A value of any component type is laid out in linear memory, flattened to core values for a
//! call, lowered into memory, and lifted back. One walker covers every type, recursively: a
//! record, tuple, list, option, result or variant may hold any type, strings and lists included.
//! Memory access goes through [`Memory`], so the same code serves an instance's exports and a host
//! function that a guest calls.

use std::slice::Iter;

use tpt_wasm_types::{Value, ValueType};

use crate::{
    align_to, enum_case, ComponentType, ComponentTypeDef, ComponentValue, RecordField, Scalar,
    StringEncoding, MAX_STRING_BYTE_LENGTH, UTF16_TAG,
};

/// The linear memory a value is lowered into and lifted from, and the `realloc` that allocates in
/// it.
pub(crate) trait Memory {
    /// `length` bytes at `address`.
    fn read(&mut self, address: u32, length: u32) -> Result<Vec<u8>, String>;
    /// Write `bytes` at `address`.
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String>;
    /// `realloc(old, old size, align, new size)`: allocates or resizes a block, returning its
    /// address.
    fn realloc(
        &mut self,
        old: u32,
        old_size: u32,
        align: u32,
        new_size: u32,
    ) -> Result<u32, String>;
}

/// Whether a type holds a string or a list anywhere inside it, so that a value of it lives in
/// memory and needs one, and a `realloc`, to be lowered.
pub(crate) fn needs_memory(ty: &ComponentType, types: &[ComponentTypeDef]) -> bool {
    match *ty {
        ComponentType::Scalar(Scalar::String) | ComponentType::List(_) => true,
        ComponentType::Scalar(_) | ComponentType::Enum(_) | ComponentType::Flags(_) => false,
        ComponentType::Record(index) => match definition(types, index) {
            Some(ComponentTypeDef::Record(fields)) => {
                fields.iter().any(|field| needs_memory(&field.ty, types))
            }
            _ => false,
        },
        ComponentType::Tuple(index) => match definition(types, index) {
            Some(ComponentTypeDef::Tuple(items)) => {
                items.iter().any(|item| needs_memory(item, types))
            }
            _ => false,
        },
        ComponentType::Option(index) => match definition(types, index) {
            Some(ComponentTypeDef::Option(payload)) => needs_memory(payload, types),
            _ => false,
        },
        ComponentType::Result(index) => match definition(types, index) {
            Some(ComponentTypeDef::Result { ok, err }) => ok
                .iter()
                .chain(err.iter())
                .any(|payload| needs_memory(payload, types)),
            _ => false,
        },
        ComponentType::Variant(index) => match definition(types, index) {
            Some(ComponentTypeDef::Variant { cases }) => cases
                .iter()
                .filter_map(|(_, payload)| payload.as_ref())
                .any(|payload| needs_memory(payload, types)),
            _ => false,
        },
    }
}

/// The byte width of a discriminant that names one of `count` cases.
pub(crate) fn discriminant_size(count: usize) -> usize {
    match count {
        0..=256 => 1,
        257..=65536 => 2,
        _ => 4,
    }
}

/// The definition a type index names, if the table has one.
fn definition(types: &[ComponentTypeDef], index: u32) -> Option<&ComponentTypeDef> {
    types.get(index as usize)
}

fn record_fields(types: &[ComponentTypeDef], index: u32) -> Result<&[RecordField], String> {
    match definition(types, index) {
        Some(ComponentTypeDef::Record(fields)) => Ok(fields),
        _ => Err("the record is not defined".to_owned()),
    }
}

fn list_element(types: &[ComponentTypeDef], index: u32) -> Result<ComponentType, String> {
    match definition(types, index) {
        Some(ComponentTypeDef::List(element)) => Ok(*element),
        _ => Err("the list is not defined".to_owned()),
    }
}

/// `base` moved on by `by` bytes, refusing an address past the 32-bit space.
fn offset_address(base: u32, by: usize) -> Result<u32, String> {
    u32::try_from(by)
        .ok()
        .and_then(|by| base.checked_add(by))
        .ok_or_else(|| "a value is laid out past the end of memory".to_owned())
}

/// The offsets of values of `types` laid out one after another, and the size and alignment of the
/// whole, by the canonical record rules.
pub(crate) fn layout(
    types: &[ComponentType],
    defined: &[ComponentTypeDef],
) -> (Vec<usize>, usize, usize) {
    let mut offsets = Vec::with_capacity(types.len());
    let mut size = 0usize;
    let mut align = 1usize;
    for ty in types {
        let (field_size, field_align) = size_align(ty, defined);
        align = align.max(field_align);
        let offset = align_to(size, field_align);
        offsets.push(offset);
        size = offset + field_size;
    }
    (offsets, align_to(size, align), align)
}

/// The largest alignment among the payloads of a set of cases, at least one byte.
fn payload_align(payloads: &[Option<ComponentType>], types: &[ComponentTypeDef]) -> usize {
    payloads
        .iter()
        .flatten()
        .map(|payload| size_align(payload, types).1)
        .fold(1, usize::max)
}

/// The size and alignment of a set of cases, each holding an optional payload. The case index
/// comes first, and the payloads follow at the offset their widest alignment allows.
fn case_layout(payloads: &[Option<ComponentType>], types: &[ComponentTypeDef]) -> (usize, usize) {
    let discriminant = discriminant_size(payloads.len());
    let payload_size = payloads
        .iter()
        .flatten()
        .map(|payload| size_align(payload, types).0)
        .fold(0, usize::max);
    let payload_align = payload_align(payloads, types);
    let align = discriminant.max(payload_align);
    (
        align_to(align_to(discriminant, payload_align) + payload_size, align),
        align,
    )
}

/// The size and alignment of a value of type `ty` in linear memory.
pub(crate) fn size_align(ty: &ComponentType, types: &[ComponentTypeDef]) -> (usize, usize) {
    match *ty {
        ComponentType::Scalar(scalar) => scalar.size_and_align(),
        // A list in memory is its pointer and its length.
        ComponentType::List(_) => (8, 4),
        ComponentType::Record(index) => match definition(types, index) {
            Some(ComponentTypeDef::Record(fields)) => {
                let tys: Vec<ComponentType> = fields.iter().map(|field| field.ty).collect();
                let (_, size, align) = layout(&tys, types);
                (size, align)
            }
            _ => (0, 1),
        },
        ComponentType::Tuple(index) => match definition(types, index) {
            Some(ComponentTypeDef::Tuple(items)) => {
                let (_, size, align) = layout(items, types);
                (size, align)
            }
            _ => (0, 1),
        },
        ComponentType::Enum(index) => match definition(types, index) {
            Some(ComponentTypeDef::Enum(labels)) => {
                let size = discriminant_size(labels.len());
                (size, size)
            }
            _ => (1, 1),
        },
        ComponentType::Flags(index) => match definition(types, index) {
            Some(ComponentTypeDef::Flags(labels)) => {
                let size = match labels.len() {
                    0..=8 => 1,
                    9..=16 => 2,
                    _ => 4,
                };
                (size, size)
            }
            _ => (1, 1),
        },
        ComponentType::Option(index) => match definition(types, index) {
            Some(ComponentTypeDef::Option(payload)) => case_layout(&[None, Some(*payload)], types),
            _ => (1, 1),
        },
        ComponentType::Result(index) => match definition(types, index) {
            Some(ComponentTypeDef::Result { ok, err }) => case_layout(&[*ok, *err], types),
            _ => (1, 1),
        },
        ComponentType::Variant(index) => match definition(types, index) {
            Some(ComponentTypeDef::Variant { cases }) => {
                let payloads: Vec<Option<ComponentType>> =
                    cases.iter().map(|(_, payload)| *payload).collect();
                case_layout(&payloads, types)
            }
            _ => (1, 1),
        },
    }
}

/// The core values a type flattens to in a call.
pub(crate) fn flat(ty: &ComponentType, types: &[ComponentTypeDef]) -> Vec<ValueType> {
    match *ty {
        ComponentType::Scalar(scalar) => scalar.flat_params(),
        ComponentType::List(_) => vec![ValueType::I32, ValueType::I32],
        ComponentType::Record(index) => match definition(types, index) {
            Some(ComponentTypeDef::Record(fields)) => fields
                .iter()
                .flat_map(|field| flat(&field.ty, types))
                .collect(),
            _ => Vec::new(),
        },
        ComponentType::Tuple(index) => match definition(types, index) {
            Some(ComponentTypeDef::Tuple(items)) => {
                items.iter().flat_map(|item| flat(item, types)).collect()
            }
            _ => Vec::new(),
        },
        ComponentType::Enum(_) | ComponentType::Flags(_) => vec![ValueType::I32],
        ComponentType::Option(index) => match definition(types, index) {
            Some(ComponentTypeDef::Option(payload)) => variant_flat(&[None, Some(*payload)], types),
            _ => variant_flat(&[], types),
        },
        ComponentType::Result(index) => match definition(types, index) {
            Some(ComponentTypeDef::Result { ok, err }) => variant_flat(&[*ok, *err], types),
            _ => variant_flat(&[], types),
        },
        ComponentType::Variant(index) => match definition(types, index) {
            Some(ComponentTypeDef::Variant { cases }) => {
                let payloads: Vec<Option<ComponentType>> =
                    cases.iter().map(|(_, payload)| *payload).collect();
                variant_flat(&payloads, types)
            }
            _ => variant_flat(&[], types),
        },
    }
}

/// A case index, then the joined flat values of the payloads.
fn variant_flat(payloads: &[Option<ComponentType>], types: &[ComponentTypeDef]) -> Vec<ValueType> {
    let mut flat_values = vec![ValueType::I32];
    flat_values.extend(joined(payloads, types));
    flat_values
}

/// The flat types that the payloads of a set of cases share: each position holds the join of the
/// types the cases place there.
fn joined(payloads: &[Option<ComponentType>], types: &[ComponentTypeDef]) -> Vec<ValueType> {
    let per_case: Vec<Vec<ValueType>> = payloads
        .iter()
        .map(|payload| match payload {
            Some(ty) => flat(ty, types),
            None => Vec::new(),
        })
        .collect();
    let width = per_case.iter().map(Vec::len).max().unwrap_or(0);
    (0..width)
        .map(|position| {
            per_case
                .iter()
                .filter_map(|case| case.get(position).copied())
                .reduce(join)
                .unwrap_or(ValueType::I32)
        })
        .collect()
}

/// The join of two flat types, as the canonical ABI gives it: equal types stay, `i32` with `f32`
/// is `i32`, and any other pair is `i64`.
pub(crate) fn join(a: ValueType, b: ValueType) -> ValueType {
    if a == b {
        a
    } else if matches!(
        (a, b),
        (ValueType::I32, ValueType::F32) | (ValueType::F32, ValueType::I32)
    ) {
        ValueType::I32
    } else {
        ValueType::I64
    }
}

/// A flat value widened to the joined type it is placed as.
fn coerce(value: &Value, to: ValueType) -> Result<Value, String> {
    Ok(match (value, to) {
        (Value::I32(value), ValueType::I32) => Value::I32(*value),
        (Value::I32(value), ValueType::I64) => Value::I64(i64::from(*value as u32)),
        (Value::I64(value), ValueType::I64) => Value::I64(*value),
        (Value::F32(bits), ValueType::F32) => Value::F32(*bits),
        (Value::F32(bits), ValueType::I32) => Value::I32(*bits as i32),
        (Value::F32(bits), ValueType::I64) => Value::I64(i64::from(*bits)),
        (Value::F64(bits), ValueType::F64) => Value::F64(*bits),
        (Value::F64(bits), ValueType::I64) => Value::I64(*bits as i64),
        _ => return Err("a payload's flat value does not join with its case".to_owned()),
    })
}

/// A joined flat value narrowed back to the type a case's payload has.
fn narrow(value: &Value, to: ValueType) -> Result<Value, String> {
    Ok(match (value, to) {
        (Value::I32(value), ValueType::I32) => Value::I32(*value),
        (Value::I32(value), ValueType::F32) => Value::F32(*value as u32),
        (Value::I64(value), ValueType::I64) => Value::I64(*value),
        (Value::I64(value), ValueType::I32) => Value::I32(*value as i32),
        (Value::I64(value), ValueType::F32) => Value::F32(*value as u32),
        (Value::I64(value), ValueType::F64) => Value::F64(*value as u64),
        (Value::F32(bits), ValueType::F32) => Value::F32(*bits),
        (Value::F64(bits), ValueType::F64) => Value::F64(*bits),
        _ => return Err("a joined flat value does not narrow to its case".to_owned()),
    })
}

/// The zero core value of a flat type, which fills a position a case does not use.
fn zero_value(ty: ValueType) -> Value {
    match ty {
        ValueType::I64 => Value::I64(0),
        ValueType::F32 => Value::F32(0),
        ValueType::F64 => Value::F64(0),
        _ => Value::I32(0),
    }
}

/// Lower a value to its flat core values. A string or a list is copied into memory first, and
/// lowers to its pointer and length.
pub(crate) fn lower(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    value: &ComponentValue,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
) -> Result<Vec<Value>, String> {
    let mismatch = || "a value does not match its type".to_owned();
    match (value, *ty) {
        (ComponentValue::String(text), ComponentType::Scalar(Scalar::String)) => {
            let (pointer, length) = lower_string(memory, encoding, text)?;
            Ok(vec![Value::I32(pointer as i32), Value::I32(length as i32)])
        }
        (ComponentValue::List(items), ComponentType::List(index)) => {
            let element = list_element(types, index)?;
            let (pointer, length) = lower_list(memory, encoding, items, &element, types)?;
            Ok(vec![Value::I32(pointer as i32), Value::I32(length as i32)])
        }
        (ComponentValue::Record(values), ComponentType::Record(index)) => {
            let defined = record_fields(types, index)?;
            if values.len() != defined.len() {
                return Err(mismatch());
            }
            let mut lowered = Vec::new();
            for ((name, value), field) in values.iter().zip(defined) {
                if *name != field.name {
                    return Err(mismatch());
                }
                lowered.extend(lower(memory, encoding, value, &field.ty, types)?);
            }
            Ok(lowered)
        }
        (ComponentValue::Tuple(items), ComponentType::Tuple(index)) => {
            let Some(ComponentTypeDef::Tuple(defined)) = definition(types, index) else {
                return Err(mismatch());
            };
            if items.len() != defined.len() {
                return Err(mismatch());
            }
            let mut lowered = Vec::new();
            for (item, ty) in items.iter().zip(defined) {
                lowered.extend(lower(memory, encoding, item, ty, types)?);
            }
            Ok(lowered)
        }
        (ComponentValue::Enum(label), ComponentType::Enum(index)) => {
            let Some(ComponentTypeDef::Enum(labels)) = definition(types, index) else {
                return Err(mismatch());
            };
            let case = labels
                .iter()
                .position(|candidate| candidate == label)
                .ok_or_else(mismatch)?;
            Ok(vec![Value::I32(case as i32)])
        }
        (ComponentValue::Flags(set), ComponentType::Flags(index)) => {
            let Some(ComponentTypeDef::Flags(defined)) = definition(types, index) else {
                return Err(mismatch());
            };
            let mut bits: u32 = 0;
            for label in set {
                let position = defined
                    .iter()
                    .position(|candidate| candidate == label)
                    .ok_or_else(mismatch)?;
                bits |= 1u32 << position;
            }
            Ok(vec![Value::I32(bits as i32)])
        }
        (ComponentValue::Option(payload), ComponentType::Option(index)) => {
            let Some(ComponentTypeDef::Option(element)) = definition(types, index) else {
                return Err(mismatch());
            };
            let payloads = [None, Some(*element)];
            match payload.as_deref() {
                None => lower_case(memory, encoding, 0, None, &payloads, types),
                Some(value) => lower_case(memory, encoding, 1, Some(value), &payloads, types),
            }
        }
        (ComponentValue::Result(outcome), ComponentType::Result(index)) => {
            let Some(ComponentTypeDef::Result { ok, err }) = definition(types, index) else {
                return Err(mismatch());
            };
            let payloads = [*ok, *err];
            let (case, payload) = match outcome {
                Ok(payload) => (0, payload.as_deref()),
                Err(payload) => (1, payload.as_deref()),
            };
            lower_case(memory, encoding, case, payload, &payloads, types)
        }
        (ComponentValue::Variant(label, payload), ComponentType::Variant(index)) => {
            let Some(ComponentTypeDef::Variant { cases }) = definition(types, index) else {
                return Err(mismatch());
            };
            let case = cases
                .iter()
                .position(|(name, _)| name == label)
                .ok_or_else(mismatch)?;
            let payloads: Vec<Option<ComponentType>> =
                cases.iter().map(|(_, payload)| *payload).collect();
            lower_case(memory, encoding, case, payload.as_deref(), &payloads, types)
        }
        (value, ComponentType::Scalar(scalar)) if scalar != Scalar::String => {
            if value.scalar() != Some(scalar) {
                return Err(mismatch());
            }
            lower_scalar(value)
                .map(|lowered| vec![lowered])
                .ok_or_else(mismatch)
        }
        _ => Err(mismatch()),
    }
}

/// The flat values of a case of a variant-like value: its case index, then the joined payload
/// values, with the chosen payload's values in its own positions and zeros elsewhere.
fn lower_case(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    case: usize,
    payload: Option<&ComponentValue>,
    payloads: &[Option<ComponentType>],
    types: &[ComponentTypeDef],
) -> Result<Vec<Value>, String> {
    let own = match (payloads.get(case).copied().flatten(), payload) {
        (None, None) => Vec::new(),
        (Some(ty), Some(value)) => lower(memory, encoding, value, &ty, types)?,
        _ => return Err("a value does not match its case".to_owned()),
    };
    let mut lowered = vec![Value::I32(case as i32)];
    for (position, ty) in joined(payloads, types).into_iter().enumerate() {
        match own.get(position) {
            Some(value) => lowered.push(coerce(value, ty)?),
            None => lowered.push(zero_value(ty)),
        }
    }
    Ok(lowered)
}

/// Lift a value from its flat core values, taking as many as its type flattens to.
pub(crate) fn lift(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    values: &mut Iter<'_, Value>,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
) -> Result<ComponentValue, String> {
    match *ty {
        ComponentType::Scalar(Scalar::String) => {
            let (pointer, length) = two_words(values)?;
            lift_string(memory, encoding, pointer, length).map(ComponentValue::String)
        }
        ComponentType::Scalar(scalar) => {
            let value = values
                .next()
                .ok_or_else(|| "a value is missing its core value".to_owned())?;
            lift_scalar(value.clone(), scalar)
        }
        ComponentType::List(index) => {
            let element = list_element(types, index)?;
            let (pointer, length) = two_words(values)?;
            lift_list(memory, encoding, pointer, length, &element, types).map(ComponentValue::List)
        }
        ComponentType::Record(index) => {
            let defined = record_fields(types, index)?;
            let mut fields = Vec::with_capacity(defined.len());
            for field in defined {
                let value = lift(memory, encoding, values, &field.ty, types)?;
                fields.push((field.name.clone(), value));
            }
            Ok(ComponentValue::Record(fields))
        }
        ComponentType::Tuple(index) => {
            let Some(ComponentTypeDef::Tuple(defined)) = definition(types, index) else {
                return Err("the tuple is not defined".to_owned());
            };
            let mut items = Vec::with_capacity(defined.len());
            for ty in defined {
                items.push(lift(memory, encoding, values, ty, types)?);
            }
            Ok(ComponentValue::Tuple(items))
        }
        ComponentType::Enum(index) => {
            let Some(ComponentTypeDef::Enum(labels)) = definition(types, index) else {
                return Err("the enum is not defined".to_owned());
            };
            let value = values
                .next()
                .ok_or_else(|| "a value is missing its case".to_owned())?;
            let case = enum_case(value, labels.len())?;
            Ok(ComponentValue::Enum(labels[case].clone()))
        }
        ComponentType::Flags(index) => {
            let Some(ComponentTypeDef::Flags(labels)) = definition(types, index) else {
                return Err("the flags are not defined".to_owned());
            };
            let Some(Value::I32(bits)) = values.next() else {
                return Err("a flags value is not an i32".to_owned());
            };
            flags_from_bits(*bits as u32, labels)
        }
        ComponentType::Option(index) => {
            let Some(ComponentTypeDef::Option(payload)) = definition(types, index) else {
                return Err("the option is not defined".to_owned());
            };
            let payloads = [None, Some(*payload)];
            let (_, value) = lift_case(memory, encoding, values, &payloads, types)?;
            Ok(ComponentValue::Option(value.map(Box::new)))
        }
        ComponentType::Result(index) => {
            let Some(ComponentTypeDef::Result { ok, err }) = definition(types, index) else {
                return Err("the result is not defined".to_owned());
            };
            let payloads = [*ok, *err];
            let (case, value) = lift_case(memory, encoding, values, &payloads, types)?;
            let value = value.map(Box::new);
            Ok(ComponentValue::Result(if case == 0 {
                Ok(value)
            } else {
                Err(value)
            }))
        }
        ComponentType::Variant(index) => {
            let Some(ComponentTypeDef::Variant { cases }) = definition(types, index) else {
                return Err("the variant is not defined".to_owned());
            };
            let payloads: Vec<Option<ComponentType>> =
                cases.iter().map(|(_, payload)| *payload).collect();
            let (case, value) = lift_case(memory, encoding, values, &payloads, types)?;
            Ok(ComponentValue::Variant(
                cases[case].0.clone(),
                value.map(Box::new),
            ))
        }
    }
}

/// Lift the case of a variant-like value from its flat values: the case index, then the joined
/// payload values, from which the chosen case's payload is narrowed.
fn lift_case(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    values: &mut Iter<'_, Value>,
    payloads: &[Option<ComponentType>],
    types: &[ComponentTypeDef],
) -> Result<(usize, Option<ComponentValue>), String> {
    let case = match values.next() {
        Some(Value::I32(case)) => *case as u32 as usize,
        _ => return Err("a variant's case index is not an i32".to_owned()),
    };
    if case >= payloads.len() {
        return Err(format!(
            "case {case} is not one of {} cases",
            payloads.len()
        ));
    }
    let joined_types = joined(payloads, types);
    let mut joined_values = Vec::with_capacity(joined_types.len());
    for _ in &joined_types {
        joined_values.push(
            values
                .next()
                .cloned()
                .ok_or_else(|| "a variant is missing its payload values".to_owned())?,
        );
    }
    let payload = match payloads[case] {
        None => None,
        Some(ty) => {
            let target = flat(&ty, types);
            let mut narrowed = Vec::with_capacity(target.len());
            for (value, want) in joined_values.iter().zip(&target) {
                narrowed.push(narrow(value, *want)?);
            }
            Some(lift(memory, encoding, &mut narrowed.iter(), &ty, types)?)
        }
    };
    Ok((case, payload))
}

/// Read a set of flags from its bits, refusing a bit that names no label.
fn flags_from_bits(bits: u32, labels: &[String]) -> Result<ComponentValue, String> {
    // A bit beyond the labels names no flag, which the canonical ABI traps on.
    if labels.len() < 32 && bits >> labels.len() != 0 {
        return Err(format!("{bits:#x} sets a bit that names no flag"));
    }
    let set = labels
        .iter()
        .enumerate()
        .filter(|(position, _)| bits & (1u32 << position) != 0)
        .map(|(_, label)| label.clone())
        .collect();
    Ok(ComponentValue::Flags(set))
}

/// The two core words of a string or a list: its pointer and its length.
fn two_words(values: &mut Iter<'_, Value>) -> Result<(u32, u32), String> {
    let mut word = || match values.next() {
        Some(Value::I32(word)) => Ok(*word as u32),
        _ => Err("a string or list is not two i32 values".to_owned()),
    };
    Ok((word()?, word()?))
}

/// Store a value of type `ty` into memory at `address`, in the canonical layout. A string or a
/// list is copied into memory of its own, and its pointer and length are stored in place.
pub(crate) fn store(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    value: &ComponentValue,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
    address: u32,
) -> Result<(), String> {
    let mismatch = || "a value does not match its type".to_owned();
    match (value, *ty) {
        (ComponentValue::String(text), ComponentType::Scalar(Scalar::String)) => {
            let (pointer, length) = lower_string(memory, encoding, text)?;
            write_words(memory, address, pointer, length)
        }
        (ComponentValue::List(items), ComponentType::List(index)) => {
            let element = list_element(types, index)?;
            let (pointer, length) = lower_list(memory, encoding, items, &element, types)?;
            write_words(memory, address, pointer, length)
        }
        (value, ComponentType::Scalar(scalar)) => {
            let bytes = scalar_bytes(value, scalar).ok_or_else(mismatch)?;
            memory.write(address, &bytes)
        }
        (ComponentValue::Record(values), ComponentType::Record(index)) => {
            let defined = record_fields(types, index)?;
            if values.len() != defined.len() {
                return Err(mismatch());
            }
            let tys: Vec<ComponentType> = defined.iter().map(|field| field.ty).collect();
            let (offsets, _, _) = layout(&tys, types);
            for (((name, value), field), offset) in values.iter().zip(defined).zip(offsets) {
                if *name != field.name {
                    return Err(mismatch());
                }
                store(
                    memory,
                    encoding,
                    value,
                    &field.ty,
                    types,
                    offset_address(address, offset)?,
                )?;
            }
            Ok(())
        }
        (ComponentValue::Tuple(items), ComponentType::Tuple(index)) => {
            let Some(ComponentTypeDef::Tuple(defined)) = definition(types, index) else {
                return Err(mismatch());
            };
            if items.len() != defined.len() {
                return Err(mismatch());
            }
            let (offsets, _, _) = layout(defined, types);
            for ((item, ty), offset) in items.iter().zip(defined).zip(offsets) {
                store(
                    memory,
                    encoding,
                    item,
                    ty,
                    types,
                    offset_address(address, offset)?,
                )?;
            }
            Ok(())
        }
        (ComponentValue::Enum(label), ComponentType::Enum(index)) => {
            let Some(ComponentTypeDef::Enum(labels)) = definition(types, index) else {
                return Err(mismatch());
            };
            let case = labels
                .iter()
                .position(|candidate| candidate == label)
                .ok_or_else(mismatch)?;
            let size = discriminant_size(labels.len());
            memory.write(address, &(case as u32).to_le_bytes()[..size])
        }
        (ComponentValue::Flags(set), ComponentType::Flags(index)) => {
            let Some(ComponentTypeDef::Flags(defined)) = definition(types, index) else {
                return Err(mismatch());
            };
            let mut bits: u32 = 0;
            for label in set {
                let position = defined
                    .iter()
                    .position(|candidate| candidate == label)
                    .ok_or_else(mismatch)?;
                bits |= 1u32 << position;
            }
            let (size, _) = size_align(ty, types);
            memory.write(address, &bits.to_le_bytes()[..size])
        }
        (ComponentValue::Option(payload), ComponentType::Option(index)) => {
            let Some(ComponentTypeDef::Option(element)) = definition(types, index) else {
                return Err(mismatch());
            };
            let payloads = [None, Some(*element)];
            match payload.as_deref() {
                None => store_case(memory, encoding, address, 0, None, &payloads, types),
                Some(value) => {
                    store_case(memory, encoding, address, 1, Some(value), &payloads, types)
                }
            }
        }
        (ComponentValue::Result(outcome), ComponentType::Result(index)) => {
            let Some(ComponentTypeDef::Result { ok, err }) = definition(types, index) else {
                return Err(mismatch());
            };
            let payloads = [*ok, *err];
            let (case, payload) = match outcome {
                Ok(payload) => (0, payload.as_deref()),
                Err(payload) => (1, payload.as_deref()),
            };
            store_case(memory, encoding, address, case, payload, &payloads, types)
        }
        (ComponentValue::Variant(label, payload), ComponentType::Variant(index)) => {
            let Some(ComponentTypeDef::Variant { cases }) = definition(types, index) else {
                return Err(mismatch());
            };
            let case = cases
                .iter()
                .position(|(name, _)| name == label)
                .ok_or_else(mismatch)?;
            let payloads: Vec<Option<ComponentType>> =
                cases.iter().map(|(_, payload)| *payload).collect();
            store_case(
                memory,
                encoding,
                address,
                case,
                payload.as_deref(),
                &payloads,
                types,
            )
        }
        _ => Err(mismatch()),
    }
}

/// Store a pointer and a length, as a string or a list is stored in memory.
fn write_words(
    memory: &mut dyn Memory,
    address: u32,
    pointer: u32,
    length: u32,
) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(8);
    bytes.extend_from_slice(&pointer.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    memory.write(address, &bytes)
}

/// Store the case index of a variant-like value, then its payload at the offset its alignment
/// allows.
fn store_case(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    address: u32,
    case: usize,
    payload: Option<&ComponentValue>,
    payloads: &[Option<ComponentType>],
    types: &[ComponentTypeDef],
) -> Result<(), String> {
    let discriminant = discriminant_size(payloads.len());
    memory.write(address, &(case as u32).to_le_bytes()[..discriminant])?;
    let offset = align_to(discriminant, payload_align(payloads, types));
    match (payloads.get(case).copied().flatten(), payload) {
        (None, None) => Ok(()),
        (Some(ty), Some(value)) => store(
            memory,
            encoding,
            value,
            &ty,
            types,
            offset_address(address, offset)?,
        ),
        _ => Err("a value does not match its case".to_owned()),
    }
}

/// Load a value of type `ty` from memory at `address`, by the canonical layout.
pub(crate) fn load(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    address: u32,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
) -> Result<ComponentValue, String> {
    match *ty {
        ComponentType::Scalar(Scalar::String) => {
            let (pointer, length) = read_words(memory, address)?;
            lift_string(memory, encoding, pointer, length).map(ComponentValue::String)
        }
        ComponentType::Scalar(scalar) => {
            let (size, _) = scalar.size_and_align();
            let bytes = memory.read(address, size as u32)?;
            scalar_from_bytes(scalar, &bytes)
        }
        ComponentType::List(index) => {
            let element = list_element(types, index)?;
            let (pointer, length) = read_words(memory, address)?;
            lift_list(memory, encoding, pointer, length, &element, types).map(ComponentValue::List)
        }
        ComponentType::Record(index) => {
            let defined = record_fields(types, index)?;
            let tys: Vec<ComponentType> = defined.iter().map(|field| field.ty).collect();
            let (offsets, _, _) = layout(&tys, types);
            let mut fields = Vec::with_capacity(defined.len());
            for (field, offset) in defined.iter().zip(offsets) {
                let value = load(
                    memory,
                    encoding,
                    offset_address(address, offset)?,
                    &field.ty,
                    types,
                )?;
                fields.push((field.name.clone(), value));
            }
            Ok(ComponentValue::Record(fields))
        }
        ComponentType::Tuple(index) => {
            let Some(ComponentTypeDef::Tuple(defined)) = definition(types, index) else {
                return Err("the tuple is not defined".to_owned());
            };
            let (offsets, _, _) = layout(defined, types);
            let mut items = Vec::with_capacity(defined.len());
            for (ty, offset) in defined.iter().zip(offsets) {
                items.push(load(
                    memory,
                    encoding,
                    offset_address(address, offset)?,
                    ty,
                    types,
                )?);
            }
            Ok(ComponentValue::Tuple(items))
        }
        ComponentType::Enum(index) => {
            let Some(ComponentTypeDef::Enum(labels)) = definition(types, index) else {
                return Err("the enum is not defined".to_owned());
            };
            let case = read_word(memory, address, discriminant_size(labels.len()))?;
            let case = enum_case(&Value::I32(case as i32), labels.len())?;
            Ok(ComponentValue::Enum(labels[case].clone()))
        }
        ComponentType::Flags(index) => {
            let Some(ComponentTypeDef::Flags(labels)) = definition(types, index) else {
                return Err("the flags are not defined".to_owned());
            };
            let size = size_align(ty, types).0;
            let bits = read_word(memory, address, size)?;
            flags_from_bits(bits, labels)
        }
        ComponentType::Option(index) => {
            let Some(ComponentTypeDef::Option(payload)) = definition(types, index) else {
                return Err("the option is not defined".to_owned());
            };
            let payloads = [None, Some(*payload)];
            let (_, value) = load_case(memory, encoding, address, &payloads, types)?;
            Ok(ComponentValue::Option(value.map(Box::new)))
        }
        ComponentType::Result(index) => {
            let Some(ComponentTypeDef::Result { ok, err }) = definition(types, index) else {
                return Err("the result is not defined".to_owned());
            };
            let payloads = [*ok, *err];
            let (case, value) = load_case(memory, encoding, address, &payloads, types)?;
            let value = value.map(Box::new);
            Ok(ComponentValue::Result(if case == 0 {
                Ok(value)
            } else {
                Err(value)
            }))
        }
        ComponentType::Variant(index) => {
            let Some(ComponentTypeDef::Variant { cases }) = definition(types, index) else {
                return Err("the variant is not defined".to_owned());
            };
            let payloads: Vec<Option<ComponentType>> =
                cases.iter().map(|(_, payload)| *payload).collect();
            let (case, value) = load_case(memory, encoding, address, &payloads, types)?;
            Ok(ComponentValue::Variant(
                cases[case].0.clone(),
                value.map(Box::new),
            ))
        }
    }
}

/// Load the case of a variant-like value at `address`, and its payload if it has one.
fn load_case(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    address: u32,
    payloads: &[Option<ComponentType>],
    types: &[ComponentTypeDef],
) -> Result<(usize, Option<ComponentValue>), String> {
    let discriminant = discriminant_size(payloads.len());
    let case = read_word(memory, address, discriminant)? as usize;
    if case >= payloads.len() {
        return Err(format!(
            "case {case} is not one of {} cases",
            payloads.len()
        ));
    }
    let offset = align_to(discriminant, payload_align(payloads, types));
    let payload = match payloads[case] {
        None => None,
        Some(ty) => Some(load(
            memory,
            encoding,
            offset_address(address, offset)?,
            &ty,
            types,
        )?),
    };
    Ok((case, payload))
}

/// A little-endian word of `size` bytes at `address`, zero-extended.
fn read_word(memory: &mut dyn Memory, address: u32, size: usize) -> Result<u32, String> {
    let bytes = memory.read(address, size as u32)?;
    let mut word = [0u8; 4];
    word[..size].copy_from_slice(&bytes);
    Ok(u32::from_le_bytes(word))
}

/// The pointer and length stored at `address`.
fn read_words(memory: &mut dyn Memory, address: u32) -> Result<(u32, u32), String> {
    let header = memory.read(address, 8)?;
    Ok((
        u32::from_le_bytes([header[0], header[1], header[2], header[3]]),
        u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
    ))
}

/// Lower a string into memory in `encoding`, by the canonical ABI's `realloc` sequence. The result
/// is the pointer and the length in code units, with the Latin-1 tag bit set when the string is
/// stored as UTF-16 under `latin1+utf16`.
pub(crate) fn lower_string(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    text: &str,
) -> Result<(u32, u32), String> {
    let source_units = text.len();
    if source_units > MAX_STRING_BYTE_LENGTH {
        return Err("a string is too long for the canonical ABI".to_owned());
    }
    let (pointer, bytes, tagged) = match encoding {
        StringEncoding::Utf8 => {
            let pointer = memory.realloc(0, 0, 1, source_units as u32)?;
            (pointer, text.as_bytes().to_vec(), source_units as u32)
        }
        StringEncoding::Utf16 => {
            let worst = 2 * source_units;
            let mut pointer = memory.realloc(0, 0, 2, worst as u32)?;
            let encoded = utf16_bytes(text);
            if encoded.len() < worst {
                pointer = memory.realloc(pointer, worst as u32, 2, encoded.len() as u32)?;
            }
            let units = (encoded.len() / 2) as u32;
            (pointer, encoded, units)
        }
        StringEncoding::Latin1Utf16 => {
            let mut pointer = memory.realloc(0, 0, 2, source_units as u32)?;
            if text.chars().all(|c| (c as u32) < 256) {
                let latin1: Vec<u8> = text.chars().map(|c| c as u8).collect();
                if latin1.len() < source_units {
                    pointer =
                        memory.realloc(pointer, source_units as u32, 2, latin1.len() as u32)?;
                }
                let units = latin1.len() as u32;
                (pointer, latin1, units)
            } else {
                let worst = 2 * source_units;
                pointer = memory.realloc(pointer, source_units as u32, 2, worst as u32)?;
                let encoded = utf16_bytes(text);
                if worst > encoded.len() {
                    pointer = memory.realloc(pointer, worst as u32, 2, encoded.len() as u32)?;
                }
                let units = (encoded.len() / 2) as u32 | UTF16_TAG;
                (pointer, encoded, units)
            }
        }
    };
    memory.write(pointer, &bytes)?;
    Ok((pointer, tagged))
}

/// Read a string of `encoding` at `pointer`, whose length is `tagged_length` code units. Under
/// `latin1+utf16` the tag bit selects UTF-16. A string must be aligned, within the length limit,
/// and valid in its encoding.
pub(crate) fn lift_string(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    pointer: u32,
    tagged_length: u32,
) -> Result<String, String> {
    let (byte_length, alignment, decoding) = match encoding {
        StringEncoding::Utf8 => (u64::from(tagged_length), 1, StringDecoding::Utf8),
        StringEncoding::Utf16 => (2 * u64::from(tagged_length), 2, StringDecoding::Utf16),
        StringEncoding::Latin1Utf16 if tagged_length & UTF16_TAG != 0 => (
            2 * u64::from(tagged_length ^ UTF16_TAG),
            2,
            StringDecoding::Utf16,
        ),
        StringEncoding::Latin1Utf16 => (u64::from(tagged_length), 2, StringDecoding::Latin1),
    };
    if byte_length > MAX_STRING_BYTE_LENGTH as u64 {
        return Err("the string is too long for the canonical ABI".to_owned());
    }
    if !(pointer as usize).is_multiple_of(alignment) {
        return Err("the string is not aligned for its encoding".to_owned());
    }
    let bytes = memory.read(pointer, byte_length as u32)?;
    match decoding {
        StringDecoding::Utf8 => {
            String::from_utf8(bytes).map_err(|_| "the string is not valid UTF-8".to_owned())
        }
        StringDecoding::Utf16 => {
            // A byte length of a UTF-16 string is even, so every code unit is whole.
            let (pairs, _) = bytes.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
            String::from_utf16(&units).map_err(|_| "the string is not valid UTF-16".to_owned())
        }
        StringDecoding::Latin1 => Ok(bytes.iter().map(|&byte| char::from(byte)).collect()),
    }
}

/// Lower a list into memory: its elements are allocated with `realloc` and stored one after
/// another. The result is the pointer and the element count.
fn lower_list(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    items: &[ComponentValue],
    element: &ComponentType,
    types: &[ComponentTypeDef],
) -> Result<(u32, u32), String> {
    let (size, align) = size_align(element, types);
    let byte_length = items
        .len()
        .checked_mul(size)
        .filter(|bytes| *bytes <= MAX_STRING_BYTE_LENGTH)
        .ok_or_else(|| "a list is too long for the canonical ABI".to_owned())?;
    let length = u32::try_from(items.len())
        .map_err(|_| "a list is too long for the canonical ABI".to_owned())?;
    let pointer = memory.realloc(0, 0, align as u32, byte_length as u32)?;
    for (position, item) in items.iter().enumerate() {
        store(
            memory,
            encoding,
            item,
            element,
            types,
            offset_address(pointer, position * size)?,
        )?;
    }
    Ok((pointer, length))
}

/// Lift a list from memory: `count` elements of `element`, from `pointer`.
fn lift_list(
    memory: &mut dyn Memory,
    encoding: StringEncoding,
    pointer: u32,
    count: u32,
    element: &ComponentType,
    types: &[ComponentTypeDef],
) -> Result<Vec<ComponentValue>, String> {
    let (size, align) = size_align(element, types);
    if !(pointer as usize).is_multiple_of(align) {
        return Err("a list is not aligned for its element".to_owned());
    }
    let length = u64::from(count) * size as u64;
    if length > MAX_STRING_BYTE_LENGTH as u64 {
        return Err("a list is too long for the canonical ABI".to_owned());
    }
    let mut items = Vec::with_capacity(count as usize);
    for position in 0..count as usize {
        items.push(load(
            memory,
            encoding,
            offset_address(pointer, position * size)?,
            element,
            types,
        )?);
    }
    Ok(items)
}

/// The bytes of `text` as UTF-16, little-endian.
fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// How a string's bytes decode, once its encoding and tag are known.
enum StringDecoding {
    Utf8,
    Utf16,
    Latin1,
}

/// Whether a value is of type `ty` in the type table `types`, recursively: a record's field names
/// and order, an enum's and flags' labels, a variant's case names, and every payload must match.
pub(crate) fn matches(
    value: &ComponentValue,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
) -> bool {
    let payload_matches = |payload: &Option<Box<ComponentValue>>,
                           expected: Option<ComponentType>| {
        match (payload.as_deref(), expected) {
            (None, None) => true,
            (Some(value), Some(ty)) => matches(value, &ty, types),
            _ => false,
        }
    };
    match (value, *ty) {
        (ComponentValue::String(_), ComponentType::Scalar(Scalar::String)) => true,
        (value, ComponentType::Scalar(scalar)) => value.scalar() == Some(scalar),
        (ComponentValue::List(items), ComponentType::List(index)) => match definition(types, index)
        {
            Some(ComponentTypeDef::List(element)) => {
                items.iter().all(|item| matches(item, element, types))
            }
            _ => false,
        },
        (ComponentValue::Record(values), ComponentType::Record(index)) => {
            match definition(types, index) {
                Some(ComponentTypeDef::Record(defined)) => {
                    values.len() == defined.len()
                        && values.iter().zip(defined).all(|((name, value), field)| {
                            *name == field.name && matches(value, &field.ty, types)
                        })
                }
                _ => false,
            }
        }
        (ComponentValue::Tuple(items), ComponentType::Tuple(index)) => {
            match definition(types, index) {
                Some(ComponentTypeDef::Tuple(defined)) => {
                    items.len() == defined.len()
                        && items
                            .iter()
                            .zip(defined)
                            .all(|(item, ty)| matches(item, ty, types))
                }
                _ => false,
            }
        }
        (ComponentValue::Enum(label), ComponentType::Enum(index)) => match definition(types, index)
        {
            Some(ComponentTypeDef::Enum(labels)) => labels.contains(label),
            _ => false,
        },
        (ComponentValue::Flags(set), ComponentType::Flags(index)) => match definition(types, index)
        {
            Some(ComponentTypeDef::Flags(labels)) => set.iter().all(|label| labels.contains(label)),
            _ => false,
        },
        (ComponentValue::Option(payload), ComponentType::Option(index)) => {
            match definition(types, index) {
                Some(ComponentTypeDef::Option(element)) => match payload.as_deref() {
                    None => true,
                    Some(value) => matches(value, element, types),
                },
                _ => false,
            }
        }
        (ComponentValue::Result(outcome), ComponentType::Result(index)) => {
            match definition(types, index) {
                Some(ComponentTypeDef::Result { ok, err }) => match outcome {
                    Ok(payload) => payload_matches(payload, *ok),
                    Err(payload) => payload_matches(payload, *err),
                },
                _ => false,
            }
        }
        (ComponentValue::Variant(label, payload), ComponentType::Variant(index)) => {
            match definition(types, index) {
                Some(ComponentTypeDef::Variant { cases }) => {
                    match cases.iter().find(|(name, _)| name == label) {
                        Some((_, expected)) => payload_matches(payload, *expected),
                        None => false,
                    }
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether two types are the same, each judged in its own type table. The comparison is
/// structural: the indices that name definitions may differ, but labels, field names, case names,
/// and payload types must match. A type that names no valid definition is never the same.
pub(crate) fn same_type(
    a_types: &[ComponentTypeDef],
    x: &ComponentType,
    b_types: &[ComponentTypeDef],
    y: &ComponentType,
) -> bool {
    let same_optional = |a: Option<ComponentType>, b: Option<ComponentType>| match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_type(a_types, &a, b_types, &b),
        _ => false,
    };
    match (*x, *y) {
        (ComponentType::Scalar(a), ComponentType::Scalar(b)) => a == b,
        (ComponentType::List(i), ComponentType::List(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (Some(ComponentTypeDef::List(a)), Some(ComponentTypeDef::List(b))) => {
                    same_type(a_types, a, b_types, b)
                }
                _ => false,
            }
        }
        (ComponentType::Option(i), ComponentType::Option(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (Some(ComponentTypeDef::Option(a)), Some(ComponentTypeDef::Option(b))) => {
                    same_type(a_types, a, b_types, b)
                }
                _ => false,
            }
        }
        (ComponentType::Record(i), ComponentType::Record(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (Some(ComponentTypeDef::Record(a)), Some(ComponentTypeDef::Record(b))) => {
                    a.len() == b.len()
                        && a.iter().zip(b).all(|(fa, fb)| {
                            fa.name == fb.name && same_type(a_types, &fa.ty, b_types, &fb.ty)
                        })
                }
                _ => false,
            }
        }
        (ComponentType::Tuple(i), ComponentType::Tuple(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (Some(ComponentTypeDef::Tuple(a)), Some(ComponentTypeDef::Tuple(b))) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|(x, y)| same_type(a_types, x, b_types, y))
                }
                _ => false,
            }
        }
        (ComponentType::Enum(i), ComponentType::Enum(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (Some(ComponentTypeDef::Enum(a)), Some(ComponentTypeDef::Enum(b))) => a == b,
                _ => false,
            }
        }
        (ComponentType::Flags(i), ComponentType::Flags(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (Some(ComponentTypeDef::Flags(a)), Some(ComponentTypeDef::Flags(b))) => a == b,
                _ => false,
            }
        }
        (ComponentType::Result(i), ComponentType::Result(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (
                    Some(ComponentTypeDef::Result { ok: ao, err: ae }),
                    Some(ComponentTypeDef::Result { ok: bo, err: be }),
                ) => same_optional(*ao, *bo) && same_optional(*ae, *be),
                _ => false,
            }
        }
        (ComponentType::Variant(i), ComponentType::Variant(j)) => {
            match (definition(a_types, i), definition(b_types, j)) {
                (
                    Some(ComponentTypeDef::Variant { cases: a }),
                    Some(ComponentTypeDef::Variant { cases: b }),
                ) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|((an, ap), (bn, bp))| an == bn && same_optional(*ap, *bp))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// The scalar a value lowers to as one core value, or `None` for a value that is not a single
/// scalar. Signed and unsigned integers share their bits.
pub(crate) fn lower_scalar(value: &ComponentValue) -> Option<Value> {
    match value {
        ComponentValue::S32(value) => Some(Value::I32(*value)),
        ComponentValue::U32(value) => Some(Value::I32(*value as i32)),
        ComponentValue::S64(value) => Some(Value::I64(*value)),
        ComponentValue::U64(value) => Some(Value::I64(*value as i64)),
        ComponentValue::F32(value) => Some(Value::F32(value.to_bits())),
        ComponentValue::F64(value) => Some(Value::F64(value.to_bits())),
        ComponentValue::Bool(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::S8(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::U8(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::S16(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::U16(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::Char(value) => Some(Value::I32(*value as u32 as i32)),
        _ => None,
    }
}

/// The little-endian bytes of a scalar value of type `element`, or `None` if the value is not
/// one.
fn scalar_bytes(value: &ComponentValue, element: Scalar) -> Option<Vec<u8>> {
    if value.scalar() != Some(element) {
        return None;
    }
    let (size, _) = element.size_and_align();
    match lower_scalar(value)? {
        Value::I32(value) => Some(value.to_le_bytes()[..size].to_vec()),
        Value::I64(value) => Some(value.to_le_bytes().to_vec()),
        Value::F32(bits) => Some(bits.to_le_bytes().to_vec()),
        Value::F64(bits) => Some(bits.to_le_bytes().to_vec()),
        _ => None,
    }
}

/// The scalar that `bytes`, read little-endian and zero-extended, lifts to. `bytes` is exactly the
/// scalar's size in memory.
fn scalar_from_bytes(scalar: Scalar, bytes: &[u8]) -> Result<ComponentValue, String> {
    let (size, _) = scalar.size_and_align();
    if bytes.len() != size {
        return Err(format!("{} bytes do not hold a {scalar:?}", bytes.len()));
    }
    let mut buffer = [0u8; 8];
    buffer[..size].copy_from_slice(bytes);
    let value = match scalar {
        Scalar::S64 | Scalar::U64 => Value::I64(i64::from_le_bytes(buffer)),
        Scalar::F64 => Value::F64(u64::from_le_bytes(buffer)),
        Scalar::F32 => Value::F32(u32::from_le_bytes([
            buffer[0], buffer[1], buffer[2], buffer[3],
        ])),
        _ => Value::I32(i32::from_le_bytes([
            buffer[0], buffer[1], buffer[2], buffer[3],
        ])),
    };
    lift_scalar(value, scalar)
}

/// The scalar a core value lifts to, when the core value has the scalar's type. A string is not
/// lifted from a single value: it is read from memory.
pub(crate) fn lift_scalar(value: Value, scalar: Scalar) -> Result<ComponentValue, String> {
    match (scalar, value) {
        (Scalar::S32, Value::I32(value)) => Ok(ComponentValue::S32(value)),
        (Scalar::U32, Value::I32(value)) => Ok(ComponentValue::U32(value as u32)),
        (Scalar::S64, Value::I64(value)) => Ok(ComponentValue::S64(value)),
        (Scalar::U64, Value::I64(value)) => Ok(ComponentValue::U64(value as u64)),
        (Scalar::F32, Value::F32(bits)) => Ok(ComponentValue::F32(f32::from_bits(bits))),
        (Scalar::F64, Value::F64(bits)) => Ok(ComponentValue::F64(f64::from_bits(bits))),
        // A small integer keeps the low bits of the core value, and a signed one sign-extends
        // them, as the canonical ABI's lift does. A boolean is true for any non-zero value.
        (Scalar::Bool, Value::I32(value)) => Ok(ComponentValue::Bool(value != 0)),
        (Scalar::S8, Value::I32(value)) => Ok(ComponentValue::S8(value as i8)),
        (Scalar::U8, Value::I32(value)) => Ok(ComponentValue::U8(value as u8)),
        (Scalar::S16, Value::I32(value)) => Ok(ComponentValue::S16(value as i16)),
        (Scalar::U16, Value::I32(value)) => Ok(ComponentValue::U16(value as u16)),
        (Scalar::Char, Value::I32(value)) => char::from_u32(value as u32)
            .map(ComponentValue::Char)
            .ok_or_else(|| format!("{value:#x} is not a Unicode scalar value")),
        (scalar, value) => Err(format!(
            "a core value {value:?} does not lift to {scalar:?}"
        )),
    }
}
