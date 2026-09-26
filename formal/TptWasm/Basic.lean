/-!
# Core WebAssembly value semantics

The value domain and the width invariants the rest of the model relies on.

Every definition here is stated over Lean 4's core types, so the project needs
no external dependency and builds with nothing but the pinned toolchain.
-/

namespace TptWasm

/-- The four WebAssembly value types, in the order the binary format assigns them. -/
inductive ValType where
  | i32 | i64 | f32 | f64
  deriving DecidableEq, Repr

namespace ValType

/-- Every value type, as a list, for exhaustiveness checks. -/
def all : List ValType := [.i32, .i64, .f32, .f64]

theorem all_mem (t : ValType) : t ∈ all := by
  cases t <;> simp [all]

end ValType

/-- A runtime value.

Bit patterns are modelled as integers rather than Lean floats. IEEE 754 needs
`f32`/`f64` to distinguish NaN payloads and signed zeros, none of which survive
a round trip through a mathematical real, so the model carries the exact bits
and interprets them in the operation definitions.
-/
inductive Value where
  | i32 (bits : Int)
  | i64 (bits : Int)
  | f32 (bits : Nat)
  | f64 (bits : Nat)
  deriving DecidableEq, Repr

namespace Value

/-- The type a value inhabits. -/
def type : Value -> ValType
  | .i32 _ => .i32
  | .i64 _ => .i64
  | .f32 _ => .f32
  | .f64 _ => .f64

/-- A value is well typed when its payload fits the width its constructor claims.

Signed payloads are sign-extended into an `Int`, so the bound is the usual
two's-complement range. Unsigned payloads are naturals, bounded by the width.
-/
def WellFormed : Value -> Prop
  | .i32 bits => -(2 ^ 31) <= bits ∧ bits < 2 ^ 31
  | .i64 bits => -(2 ^ 63) <= bits ∧ bits < 2 ^ 63
  | .f32 bits => bits < 2 ^ 32
  | .f64 bits => bits < 2 ^ 64

/-- A well-formed value is typed: every constructor names its own type. -/
theorem WellFormed.type {v : Value} (h : WellFormed v) :
    v.type = .i32 ∨ v.type = .i64 ∨ v.type = .f32 ∨ v.type = .f64 := by
  cases v with
  | i32 _ => exact Or.inl rfl
  | i64 _ => exact Or.inr (Or.inl rfl)
  | f32 _ => exact Or.inr (Or.inr (Or.inl rfl))
  | f64 _ => exact Or.inr (Or.inr (Or.inr rfl))

end Value

end TptWasm