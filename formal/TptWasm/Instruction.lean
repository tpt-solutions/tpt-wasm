import TptWasm.Basic

/-!
# The modeled instruction set

Mirrors `tpt_wasm_semantics::Instruction` for the subset this model covers:
constants, the parametric stack and local operators, `i32` and `i64` integer
arithmetic, comparisons, the memory instruction set, control transfer, and the
host call that makes the capability boundary observable.

Arithmetic is stated over naturals with an explicit two's-complement wrap
(`wrap32`, `wrap64`), so `i32.add` is total and the proof obligation is about
the wrap, not about overflow being undefined.
-/

namespace TptWasm

/-- The trap conditions the model can raise. -/
inductive Trap where
  | unreachable
  | integerDivisionByZero
  | integerOverflow
  | invalidConversion
  | memoryOutOfBounds
  | tableOutOfBounds
  | nullReference
  | indirectCallTypeMismatch
  | callDepthExceeded
  | hostFailure
  deriving DecidableEq, Repr

/-- Reduce an integer into the two's-complement range of `bits` bits. -/
def wrap (bits : Nat) (value : Int) : Int :=
  let modulus := (2 : Int) ^ bits
  let r := value % modulus
  if r < 0 then r + modulus else r

/-- `i32` wraparound. -/
def wrap32 (value : Int) : Int := wrap 32 value

/-- `i64` wraparound. -/
def wrap64 (value : Int) : Int := wrap 64 value

/-- The `i32` binary operators. -/
inductive IBinOp where
  | add | sub | mul | divS | divU | remS | remU
  | and | or | xor | shl | shrS | shrU | rotl | rotr
  deriving DecidableEq, Repr

/-- The `i64` binary operators. -/
inductive I64BinOp where
  | add | sub | mul | divS | divU | remS | remU
  | and | or | xor | shl | shrS | shrU | rotl | rotr
  deriving DecidableEq, Repr

/-- The integer unary operators. -/
inductive IUnOp where
  | clz | ctz | popcnt | eqz
  deriving DecidableEq, Repr

/-- The integer comparison operators. -/
inductive ICmp where
  | eq | ne | ltS | ltU | gtS | gtU | leS | leU | geS | geU
  deriving DecidableEq, Repr

/-- A memory access immediate. -/
structure MemArg where
  offset : Nat
  deriving DecidableEq, Repr, Inhabited

/-- The fourteen MVP load operations, with the width each one touches. -/
inductive LoadOp where
  | i32 | i64 | f32 | f64
  | i32_8s | i32_8u | i32_16s | i32_16u
  | i64_8s | i64_8u | i64_16s | i64_16u | i64_32s | i64_32u
  deriving DecidableEq, Repr

namespace LoadOp

/-- Bytes touched by a load. -/
def width : LoadOp → Nat
  | .i32_8s | .i32_8u | .i64_8s | .i64_8u => 1
  | .i32_16s | .i32_16u | .i64_16s | .i64_16u => 2
  | .i32 | .f32 | .i64_32s | .i64_32u => 4
  | .i64 | .f64 => 8

/-- The value type a completed load pushes. -/
def resultType : LoadOp → ValType
  | .i32 | .i32_8s | .i32_8u | .i32_16s | .i32_16u => .i32
  | .f32 => .f32
  | .i64 | .i64_8s | .i64_8u | .i64_16s | .i64_16u | .i64_32s | .i64_32u => .i64
  | .f64 => .f64

/-- Whether a load sign-extends its narrow result. -/
def signed : LoadOp → Bool
  | .i32_8s | .i32_16s | .i64_8s | .i64_16s | .i64_32s => true
  | _ => false

end LoadOp

/-- The nine MVP store operations. -/
inductive StoreOp where
  | i32 | i64 | f32 | f64 | i32_8 | i32_16 | i64_8 | i64_16 | i64_32
  deriving DecidableEq, Repr

namespace StoreOp

/-- Bytes touched by a store. -/
def width : StoreOp → Nat
  | .i32_8 | .i64_8 => 1
  | .i32_16 | .i64_16 => 2
  | .i32 | .f32 | .i64_32 => 4
  | .i64 | .f64 => 8

/-- The operand type a store pops.

Width and value type are independent: `i32.store8` writes one byte of an `i32`
operand, and `f64.store` writes eight bytes of an `f64` operand.
-/
def operandType : StoreOp → ValType
  | .i32 | .i32_8 | .i32_16 => .i32
  | .i64 | .i64_8 | .i64_16 | .i64_32 => .i64
  | .f32 => .f32
  | .f64 => .f64

end StoreOp

/-- The modeled instruction set. -/
inductive Instr where
  | unreachable
  | nop
  | i32Const (value : Int)
  | i64Const (value : Int)
  | f32Const (bits : Nat)
  | f64Const (bits : Nat)
  | drop
  | select (ty : ValType)
  | localGet (index : Nat)
  | localSet (index : Nat)
  | localTee (index : Nat)
  | globalGet (index : Nat)
  | globalSet (index : Nat)
  | i32Un (op : IUnOp)
  | i64Un (op : IUnOp)
  | i32Bin (op : IBinOp)
  | i64Bin (op : I64BinOp)
  | i32Cmp (op : ICmp)
  | i64Cmp (op : ICmp)
  | i32WrapI64
  | i64ExtendI32S
  | i64ExtendI32U
  | i32ReinterpretF32
  | i64ReinterpretF64
  | f32ReinterpretI32
  | f64ReinterpretI64
  | load (op : LoadOp) (memory : Nat) (arg : MemArg)
  | store (op : StoreOp) (memory : Nat) (arg : MemArg)
  | memorySize (memory : Nat)
  | memoryGrow (memory : Nat)
  | call (index : Nat)
  | callHost (capability : String)
  | ret
  | endInstr
  deriving Repr

/-- The default value a local or global of a given type starts at. -/
def defaultValue : ValType → Value
  | .i32 => .i32 0
  | .i64 => .i64 0
  | .f32 => .f32 0
  | .f64 => .f64 0

end TptWasm
