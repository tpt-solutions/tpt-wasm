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

/-- Reduce an integer into the two's-complement range of `bits` bits, that is into
`[-2^(bits-1), 2^(bits-1))`, the range `Value.i32` and `Value.i64` are stated over. -/
def wrap (bits : Nat) (value : Int) : Int :=
  let modulus := (2 : Int) ^ bits
  let r := value % modulus
  if r < (2 : Int) ^ (bits - 1) then r else r - modulus

/-- The unsigned bit pattern of `value` in `bits` bits, as a natural number in `[0, 2^bits)`.
A bitwise operator works on this pattern, not on the magnitude of its operand: the magnitude
of `-1` is `1`, while its bit pattern is all ones. -/
def unsignedBits (bits : Nat) (value : Int) : Nat :=
  (value % (2 : Int) ^ bits).toNat

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

/-- The effective address of a memory access: the operand's unsigned 32-bit pattern plus the
static offset. The sum is not wrapped, so an address past `2^32` is out of bounds rather than
wrapping around to a valid one, which is what the specification requires. -/
def effectiveAddress (address : Int) (arg : MemArg) : Nat :=
  unsignedBits 32 address + arg.offset

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
  /-- A structured `if`: pops an `i32`. A non-zero value falls into the then-body, which is the
  next `thenLen` instructions. A zero value jumps past the then-body and its `elseInstr` marker,
  to the start of the else-body. The marker is not counted in `thenLen`. -/
  | ifInstr (thenLen : Nat)
  /-- The marker at the end of a then-body: control reaching it skips the `elseLen` instructions
  of the else-body. -/
  | elseInstr (elseLen : Nat)
  /-- The back-edge of a loop: control reaching it jumps back `distance` instructions from the
  back-edge itself, to the loop's condition. -/
  | loopBack (distance : Nat)
  deriving Repr

/-- The default value a local or global of a given type starts at. -/
def defaultValue : ValType → Value
  | .i32 => .i32 0
  | .i64 => .i64 0
  | .f32 => .f32 0
  | .f64 => .f64 0

end TptWasm
