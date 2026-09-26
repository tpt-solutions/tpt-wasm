import TptWasm.Basic
import TptWasm.Instruction

/-!
# Stores, memories, tables, globals, and functions

The store half of the configuration, mirroring `tpt_wasm_semantics::Store` in
the Rust model: memories hold bytes, tables hold references, globals hold one
typed value, and functions hold a signature and a body.

Memory is a `List Nat` of bytes rather than a fixed-size array, so a proof can
talk about the reachable prefix of a memory and the addresses a program may
touch without committing to a maximum size.
-/

namespace TptWasm

/-- WebAssembly page size in bytes. -/
def pageSize : Nat := 65536

/-- Abstract memory state: the bytes of one memory and its declared maximum. -/
structure MemoryState where
  bytes : List Nat
  maxPages : Nat
  deriving Repr

namespace MemoryState

/-- A memory of `pages` zeroed pages, refusing a minimum above the maximum. -/
def new (pages maxPages : Nat) : Option MemoryState :=
  if pages ≤ maxPages then some ⟨List.replicate (pages * pageSize) 0, maxPages⟩ else none

/-- The number of whole pages the memory currently holds. -/
def pages (m : MemoryState) : Nat := m.bytes.length / pageSize

/-- Whether `[address, address + width)` lies inside the memory.

This is the single bounds predicate every load and store is guarded by, so
memory safety reduces to this predicate holding whenever an access is allowed.
-/
def inBounds (m : MemoryState) (address width : Nat) : Prop :=
  address + width ≤ m.bytes.length

/-- The byte at `address`, when the single-byte access is in bounds. -/
def readByte? (m : MemoryState) (address : Nat) : Option Nat :=
  if address < m.bytes.length then m.bytes[address]? else none

/-- `bytes` with `value` written at `address`, or `none` when out of bounds.

Returning `none` rather than a truncated write is what makes a refused store
have no partial effect.
-/
def writeByte? (bytes : List Nat) (address : Nat) (value : Nat) : Option (List Nat) :=
  match bytes, address with
  | [], _ => none
  | _ :: rest, 0 => some (value :: rest)
  | head :: rest, n + 1 => (writeByte? rest n value).map (head :: ·)

/-- `bytes` with `width` bytes of `value` written little-endian at `address`.

The whole access is rejected unless every one of its bytes is in bounds, which
is exactly the V2 obligation.
-/
def writeBytesLE? (bytes : List Nat) (address width : Nat) (value : Nat) : Option (List Nat) :=
  if address + width ≤ bytes.length then
    let rec go (bs : List Nat) (a w : Nat) (v : Nat) (acc : List Nat := []) : Option (List Nat) :=
      match w with
      | 0 => some (acc ++ bs)
      | w + 1 => match bs, a with
        | [], _ => none
        | _ :: rest, 0 => go rest 1 w (v % 256) (v / 256 :: acc)
        | h :: rest, a + 1 => go rest (a + 1) w (v % 256) (v / 256 :: h :: acc)
    go bytes address width value
  else none

/-- This memory with `width` bytes of `value` written at `address`, or `none`
when the whole access is out of bounds. -/
def writeLE? (m : MemoryState) (address width value : Nat) : Option MemoryState :=
  (writeBytesLE? m.bytes address width value).map fun bytes => { m with bytes := bytes }

/-- This memory grown by `delta` pages, or `none` when that exceeds the maximum. -/
def growBy? (m : MemoryState) (delta : Nat) : Option MemoryState :=
  if m.pages + delta <= m.maxPages then
    some { m with bytes := m.bytes ++ List.replicate (delta * pageSize) 0 }
  else none

/-- The `width` bytes at `address`, read little-endian, when in bounds. -/
def readLE? (m : MemoryState) (address width : Nat) : Option Nat :=
  if address + width ≤ m.bytes.length then
    let rec go (bs : List Nat) (w : Nat) (acc : Nat) : Option Nat :=
      match w, bs with
      | 0, _ => some acc
      | _ + 1, [] => none
      | w + 1, b :: rest => go rest w (acc + b * 256 ^ w)
    go m.bytes width 0
  else none

end MemoryState

/-- Abstract table state: the references held and the declared maximum. -/
structure TableState where
  elements : List (Option Nat)
  maxElements : Nat
  deriving Repr

/-- Abstract global state: a declared type and the value it currently holds. -/
structure GlobalState where
  ty : ValType
  value : Value
  deriving Repr

/-- A function definition: its signature, its local types, and its body. -/
structure FunctionState where
  params : List ValType
  results : List ValType
  localTypes : List ValType
  body : List Instr
  deriving Repr

/-- A function reference stored in a table. -/
def funcRef (index : Nat) : Option Nat := some index

/-- The store component of an abstract configuration. -/
structure Store where
  memories : List MemoryState := []
  tables : List TableState := []
  globals : List GlobalState := []
  functions : List FunctionState := []
  deriving Repr

namespace Store

/-- The `n`-th entry of a list, or `none` when the list is too short. -/
def get? {α : Type} : List α → Nat → Option α
  | [], _ => none
  | x :: _, 0 => some x
  | _ :: xs, n + 1 => get? xs n

/-- The memory at `index`, if the store has one. -/
def memory? (s : Store) (index : Nat) : Option MemoryState :=
  get? s.memories index

/-- The table at `index`, if the store has one. -/
def table? (s : Store) (index : Nat) : Option TableState :=
  get? s.tables index

/-- The global at `index`, if the store has one. -/
def global? (s : Store) (index : Nat) : Option GlobalState :=
  get? s.globals index

/-- The function at `index`, if the store has one. -/
def function? (s : Store) (index : Nat) : Option FunctionState :=
  get? s.functions index

/-- `xs` with the `n`-th entry replaced by `x`, leaving the list alone when
`n` is past the end. -/
def set {α : Type} : List α → Nat → α → List α
  | [], _, _ => []
  | _ :: rest, 0, x => x :: rest
  | head :: rest, n + 1, x => head :: set rest n x

/-- The store with the memory at `index` replaced by `memory`. -/
def setMemory (s : Store) (index : Nat) (memory : MemoryState) : Store :=
  { s with memories := set s.memories index memory }

/-- The store with the global at `index` replaced by `global`. -/
def setGlobal (s : Store) (index : Nat) (global : GlobalState) : Store :=
  { s with globals := set s.globals index global }

/-- The store with the table at `index` replaced by `table`. -/
def setTable (s : Store) (index : Nat) (table : TableState) : Store :=
  { s with tables := set s.tables index table }

/-- A memory grown by `delta` pages, or `none` when that exceeds the maximum. -/
def growMemory? (m : MemoryState) (delta : Nat) : Option MemoryState :=
  let requested := m.pages + delta
  if requested ≤ m.maxPages then
    some { m with bytes := m.bytes ++ List.replicate (delta * pageSize) 0 }
  else none

end Store

end TptWasm
