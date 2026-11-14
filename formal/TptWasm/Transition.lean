import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Config

/-!
# Execution: the instruction semantics and the transition relation

The rules of WebAssembly Core section 4, restricted to the modeled instruction
set, in two layers.

`OpStep` is the *instruction semantics*: given an instruction and a
configuration, what it does to the operand stack, the store, and the frames.
Each constructor is one rule, so the semantics is stated declaratively and can
be reasoned about without an interpreter.

`Step` is the *machine transition*: fetch the instruction the active frame
points at, advance its program counter, and apply the instruction semantics.
Keeping the two apart is what makes the V1 correspondence provable: `execOp`
and `exec` are ordinary functions, and V1 shows the rules and the functions
compute the same thing.

Where a check fails the rule does not apply and the machine is stuck, which is
the "refuse rather than approximate" discipline used across the project.

The bit-level operators are defined on naturals and re-wrapped, because a
two's-complement reading of a negative `Int` is not available in Lean core.
`wrap32` and `wrap64` are in `TptWasm.Instruction`.
-/

namespace TptWasm

/-- The number of bits needed to write `n`, with `0` needing none. -/
def bitLengthNat (n : Nat) : Nat := if n = 0 then 0 else 1 + bitLengthNat (n / 2)
termination_by n
decreasing_by omega

/-- The number of set bits of `n`. -/
def popcountNat (n : Nat) : Nat := if n = 0 then 0 else n % 2 + popcountNat (n / 2)
termination_by n
decreasing_by omega

/-- The number of trailing zero bits of `n`: zero for an odd number, and one more for each halving
that is exact. (`0` is given `0` here; the callers handle a zero pattern themselves.) -/
def ctzNat (n : Nat) : Nat :=
  if h : n = 0 then 0
  else if n % 2 = 1 then 0
  else 1 + ctzNat (n / 2)
termination_by n
decreasing_by omega

/-- The number of leading zero bits of a `bits`-bit value, read as unsigned. -/
def clzBits (bits : Nat) (value : Int) : Nat :=
  bits - bitLengthNat (unsignedBits bits value)

/-- The number of trailing zero bits of a `bits`-bit value: `bits` for zero. -/
def ctzBits (bits : Nat) (value : Int) : Nat :=
  let pattern := unsignedBits bits value
  if pattern = 0 then bits else ctzNat pattern

/-- The number of set bits of a `bits`-bit value. -/
def popcntBits (bits : Nat) (value : Int) : Nat :=
  popcountNat (unsignedBits bits value)

/-- Unsigned comparison of two `bits`-bit values, on their bit patterns. -/
def ultBits (bits : Nat) (a b : Int) : Bool :=
  unsignedBits bits a < unsignedBits bits b

/-- The shift count of a `bits`-bit shift or rotation: the count's bit pattern, modulo the width.
A negative count is therefore not its magnitude: `-1` shifts by `bits - 1`. -/
def shiftCount (bits : Nat) (count : Int) : Nat :=
  unsignedBits bits count % bits

/-- `shl`: the value times `2^count`, wrapped. Wrapping is modulo `2^bits`, so it is the same
for a signed or an unsigned reading of the operand. -/
def shlBits (bits : Nat) (a b : Int) : Int :=
  wrap bits (a * 2 ^ shiftCount bits b)

/-- `shr_s`: arithmetic shift right, which is floor division by `2^count` (Lean's `/` on `Int`
rounds toward negative infinity, as an arithmetic shift does). -/
def shrSBits (bits : Nat) (a b : Int) : Int :=
  wrap bits (a / 2 ^ shiftCount bits b)

/-- `shr_u`: logical shift right of the bit pattern. -/
def shrUBits (bits : Nat) (a b : Int) : Int :=
  wrap bits (Int.ofNat (unsignedBits bits a / 2 ^ shiftCount bits b))

/-- `rotl` on the bit pattern. -/
def rotlBits (bits : Nat) (a b : Int) : Int :=
  let n := shiftCount bits b
  let u := unsignedBits bits a
  wrap bits (Int.ofNat ((u * 2 ^ n) % 2 ^ bits + u / 2 ^ (bits - n)))

/-- `rotr` on the bit pattern. -/
def rotrBits (bits : Nat) (a b : Int) : Int :=
  let n := shiftCount bits b
  let u := unsignedBits bits a
  wrap bits (Int.ofNat (u / 2 ^ n + (u * 2 ^ (bits - n)) % 2 ^ bits))

/-- Bitwise and of two `i32` values. -/
def iand32 (a b : Int) : Int :=
  wrap32 (Int.ofNat (Nat.land (unsignedBits 32 a) (unsignedBits 32 b)))

/-- Bitwise or of two `i32` values. -/
def ior32 (a b : Int) : Int :=
  wrap32 (Int.ofNat (Nat.lor (unsignedBits 32 a) (unsignedBits 32 b)))

/-- Bitwise exclusive or of two `i32` values. -/
def ixor32 (a b : Int) : Int :=
  wrap32 (Int.ofNat (Nat.xor (unsignedBits 32 a) (unsignedBits 32 b)))

/-- Bitwise and of two `i64` values. -/
def iand64 (a b : Int) : Int :=
  wrap64 (Int.ofNat (Nat.land (unsignedBits 64 a) (unsignedBits 64 b)))

/-- Bitwise or of two `i64` values. -/
def ior64 (a b : Int) : Int :=
  wrap64 (Int.ofNat (Nat.lor (unsignedBits 64 a) (unsignedBits 64 b)))

/-- Bitwise exclusive or of two `i64` values. -/
def ixor64 (a b : Int) : Int :=
  wrap64 (Int.ofNat (Nat.xor (unsignedBits 64 a) (unsignedBits 64 b)))

/-- Sign-extend the low `bits` bits of `raw`. -/
def signExtend (bits raw : Nat) : Int :=
  let sign := 2 ^ (bits - 1)
  if raw < sign then Int.ofNat raw else Int.ofNat raw - 2 ^ bits
/-- The `i32` binary operator.

`none` means the operation is undefined for these operands, which is exactly
the division and remainder by zero that the model reports as a trap.
-/
def evalI32Bin : IBinOp -> Int -> Int -> Option Int
  | .add, a, b => some (wrap32 (a + b))
  | .sub, a, b => some (wrap32 (a - b))
  | .mul, a, b => some (wrap32 (a * b))
  | .divS, a, b => if b = 0 then none else some (wrap32 (Int.tdiv a b))
  | .divU, a, b =>
    if b = 0 then none
    else some (wrap32 (Int.ofNat (unsignedBits 32 a / unsignedBits 32 b)))
  | .remS, a, b => if b = 0 then none else some (wrap32 (Int.tmod a b))
  | .remU, a, b =>
    if b = 0 then none
    else some (wrap32 (Int.ofNat (unsignedBits 32 a % unsignedBits 32 b)))
  | .and, a, b => some (iand32 a b)
  | .or, a, b => some (ior32 a b)
  | .xor, a, b => some (ixor32 a b)
  | .shl, a, b => some (shlBits 32 a b)
  | .shrS, a, b => some (shrSBits 32 a b)
  | .shrU, a, b => some (shrUBits 32 a b)
  | .rotl, a, b => some (rotlBits 32 a b)
  | .rotr, a, b => some (rotrBits 32 a b)

/-- The `i32` comparison, producing `1` or `0`. -/
def evalCmpBits (bits : Nat) : ICmp -> Int -> Int -> Int
  | .eq, a, b => if a = b then 1 else 0
  | .ne, a, b => if a = b then 0 else 1
  | .ltS, a, b => if a < b then 1 else 0
  | .ltU, a, b => if ultBits bits a b then 1 else 0
  | .gtS, a, b => if b < a then 1 else 0
  | .gtU, a, b => if ultBits bits b a then 1 else 0
  | .leS, a, b => if b < a then 0 else 1
  | .leU, a, b => if ultBits bits b a then 0 else 1
  | .geS, a, b => if a < b then 0 else 1
  | .geU, a, b => if ultBits bits a b then 0 else 1

/-- The `i32` comparison, producing `1` or `0`. -/
def evalI32Cmp : ICmp -> Int -> Int -> Int := evalCmpBits 32

/-- The `i32` unary operator. -/
def evalI32Un : IUnOp -> Int -> Int
  | .clz, v => Int.ofNat (clzBits 32 v)
  | .ctz, v => Int.ofNat (ctzBits 32 v)
  | .popcnt, v => Int.ofNat (popcntBits 32 v)
  | .eqz, v => if v = 0 then 1 else 0

/-- The `i64` unary operator's value: `clz`, `ctz`, and `popcnt` are `i64` results, counted at
64 bits, and `eqz` is an `i32` result, which is how WebAssembly types a test. -/
def i64UnValue : IUnOp -> Int -> Value
  | .eqz, v => .i32 (if v = 0 then 1 else 0)
  | op, v => .i64 (Int.ofNat (match op with
      | .clz => clzBits 64 v
      | .ctz => ctzBits 64 v
      | .popcnt => popcntBits 64 v
      | .eqz => 0))

/-- The `i64` binary operator, the `i32` shape at double the width.

Division and remainder are left out: the model does not represent the `i64`
divide traps, so those instructions are stuck rather than approximated.
-/
def evalI64Bin : I64BinOp -> Int -> Int -> Option Int
  | .add, a, b => some (wrap64 (a + b))
  | .sub, a, b => some (wrap64 (a - b))
  | .mul, a, b => some (wrap64 (a * b))
  | .and, a, b => some (iand64 a b)
  | .or, a, b => some (ior64 a b)
  | .xor, a, b => some (ixor64 a b)
  | .shl, a, b => some (shlBits 64 a b)
  | .shrU, a, b => some (shrUBits 64 a b)
  | _, _, _ => none

/-- The value a completed load pushes, given the raw little-endian bits.

Narrow loads are sign- or zero-extended to the width of the result type, which
is what keeps `i64.load32_s` and `i64.load32_u` distinct.
-/
def loadedValue : LoadOp -> Nat -> Value
  | .i32, raw => .i32 (wrap32 (Int.ofNat raw))
  | .i64, raw => .i64 (wrap64 (Int.ofNat raw))
  | .f32, raw => .f32 raw
  | .f64, raw => .f64 raw
  | .i32_8s, raw => .i32 (signExtend 8 raw)
  | .i32_8u, raw => .i32 (Int.ofNat raw)
  | .i32_16s, raw => .i32 (signExtend 16 raw)
  | .i32_16u, raw => .i32 (Int.ofNat raw)
  | .i64_8s, raw => .i64 (signExtend 8 raw)
  | .i64_8u, raw => .i64 (Int.ofNat raw)
  | .i64_16s, raw => .i64 (signExtend 16 raw)
  | .i64_16u, raw => .i64 (Int.ofNat raw)
  | .i64_32s, raw => .i64 (signExtend 32 raw)
  | .i64_32u, raw => .i64 (Int.ofNat raw)

/-- The bits a store writes for an operand. -/
def storedBits : Value -> Nat
  | .i32 bits => unsignedBits 32 bits
  | .i64 bits => unsignedBits 64 bits
  | .f32 bits => bits
  | .f64 bits => bits


/-- `xs` with the last `n` entries removed. -/
def dropN : List α → Nat → List α
  | [], _ => []
  | xs, 0 => xs
  | _ :: rest, n + 1 => dropN rest n

/-- The entry of `xs` at `index`, counted from the top of the stack. -/
def fromTop? : List α → Nat → Option α
  | [], _ => none
  | _, 0 => none
  | x :: _, 1 => some x
  | _ :: rest, n + 1 => fromTop? rest n

/-- The instruction semantics: what one instruction does to a configuration.

Each constructor is one rule of Core section 4 for the modeled subset.

The operand stack is written with its *top first*, so a rule that consumes `n`
operands states the stack as `v₁ :: ... :: vₙ :: rest` and produces
`(result) :: rest`. Every rule states its result directly in terms of the
configuration it was given, so a rule and the executable interpreter in this
file produce syntactically the same configuration and V1 can compare them.

A rule that reaches outside the operand stack - locals, globals, memory, calls -
carries the lookup it depends on as a premise, so a rule applies only when the
store really does contain what it names.
-/
inductive OpStep : Instr -> Configuration -> Outcome -> Prop where
  | unreachable (c : Configuration) :
      OpStep .unreachable c (.trap .unreachable)
  | nop (c : Configuration) :
      OpStep .nop c (.step c)
  | i32Const (c : Configuration) (v : Int) :
      OpStep (.i32Const v) c (.step (c.push (.i32 v)))
  | i64Const (c : Configuration) (v : Int) :
      OpStep (.i64Const v) c (.step (c.push (.i64 v)))
  | f32Const (c : Configuration) (bits : Nat) :
      OpStep (.f32Const bits) c (.step (c.push (.f32 bits)))
  | f64Const (c : Configuration) (bits : Nat) :
      OpStep (.f64Const bits) c (.step (c.push (.f64 bits)))
  | drop (c : Configuration) (rest : List Value) (value : Value)
      (stack : c.operandStack = value :: rest) :
      OpStep .drop c (.step { c with operandStack := rest })
  | select (c : Configuration) (ty : ValType) (left right : Value) (cond : Int)
      (rest : List Value) (stack : c.operandStack = (.i32 cond) :: right :: left :: rest)
      (typed : left.type = ty) (typed' : right.type = ty) :
      OpStep (.select ty) c
        (.step { c with operandStack := (if cond = 0 then right else left) :: rest })
  | localGet (c : Configuration) (frame : Frame) (index : Nat) (value : Value)
      (before after : List Value) (frameEq : c.activeFrame? = some frame)
      (locals : frame.locals = before ++ value :: after)
      (slot : index = before.length) :
      OpStep (.localGet index) c (.step (c.push value))
  | localSet (c : Configuration) (frame : Frame) (index : Nat) (old value : Value)
      (before after rest : List Value) (frameEq : c.activeFrame? = some frame)
      (stack : c.operandStack = value :: rest)
      (locals : frame.locals = before ++ old :: after)
      (slot : index = before.length) :
      OpStep (.localSet index) c
        (.step { c with
          operandStack := rest
          frames := c.frames.dropLast ++ [{ frame with locals := before ++ value :: after }] })
  | localTee (c : Configuration) (frame : Frame) (index : Nat) (old value : Value)
      (before after rest : List Value) (frameEq : c.activeFrame? = some frame)
      (stack : c.operandStack = value :: rest)
      (locals : frame.locals = before ++ old :: after)
      (slot : index = before.length) :
      OpStep (.localTee index) c
        (.step { c with
          frames := c.frames.dropLast ++ [{ frame with locals := before ++ value :: after }] })
  | globalGet (c : Configuration) (index : Nat) (global : GlobalState)
      (mem : c.store.global? index = some global) :
      OpStep (.globalGet index) c (.step (c.push global.value))
  | globalSet (c : Configuration) (index : Nat) (global : GlobalState) (value : Value)
      (rest : List Value) (stack : c.operandStack = value :: rest)
      (mem : c.store.global? index = some global)
      (typed : value.type = global.ty) :
      OpStep (.globalSet index) c
        (.step { c with
          operandStack := rest
          store := c.store.setGlobal index { global with value := value } })
  | i32Un (c : Configuration) (op : IUnOp) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i32 value :: rest) :
      OpStep (.i32Un op) c
        (.step { c with operandStack := (.i32 (evalI32Un op value)) :: rest })
  | i32Bin (c : Configuration) (op : IBinOp) (left right result : Int)
      (rest : List Value) (stack : c.operandStack = .i32 right :: .i32 left :: rest)
      (ok : evalI32Bin op left right = some result) :
      OpStep (.i32Bin op) c
        (.step { c with operandStack := (.i32 result) :: rest })
  | i32BinTrap (c : Configuration) (op : IBinOp) (left right : Int)
      (rest : List Value) (stack : c.operandStack = .i32 right :: .i32 left :: rest)
      (bad : evalI32Bin op left right = none) :
      OpStep (.i32Bin op) c (.trap .integerDivisionByZero)
  | i32Cmp (c : Configuration) (op : ICmp) (left right : Int) (rest : List Value)
      (stack : c.operandStack = .i32 right :: .i32 left :: rest) :
      OpStep (.i32Cmp op) c
        (.step { c with operandStack := (.i32 (evalI32Cmp op left right)) :: rest })
  | i64Bin (c : Configuration) (op : I64BinOp) (left right result : Int)
      (rest : List Value) (stack : c.operandStack = .i64 right :: .i64 left :: rest)
      (ok : evalI64Bin op left right = some result) :
      OpStep (.i64Bin op) c
        (.step { c with operandStack := (.i64 result) :: rest })
  | i64Un (c : Configuration) (op : IUnOp) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i64 value :: rest) :
      OpStep (.i64Un op) c
        (.step { c with operandStack := (i64UnValue op value) :: rest })
  | i64Cmp (c : Configuration) (op : ICmp) (left right : Int) (rest : List Value)
      (stack : c.operandStack = .i64 right :: .i64 left :: rest) :
      OpStep (.i64Cmp op) c
        (.step { c with operandStack := (.i32 (evalCmpBits 64 op left right)) :: rest })
  | i32WrapI64 (c : Configuration) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i64 value :: rest) :
      OpStep .i32WrapI64 c
        (.step { c with operandStack := (.i32 (wrap32 value)) :: rest })
  | i64ExtendI32S (c : Configuration) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i32 value :: rest) :
      OpStep .i64ExtendI32S c
        (.step { c with operandStack := (.i64 (wrap64 value)) :: rest })
  | i64ExtendI32U (c : Configuration) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i32 value :: rest) :
      OpStep .i64ExtendI32U c
        (.step { c with operandStack := (.i64 (Int.ofNat (unsignedBits 32 value))) :: rest })
  | i32ReinterpretF32 (c : Configuration) (bits : Nat) (rest : List Value)
      (stack : c.operandStack = .f32 bits :: rest) :
      OpStep .i32ReinterpretF32 c
        (.step { c with operandStack := (.i32 (wrap32 (Int.ofNat bits))) :: rest })
  | i64ReinterpretF64 (c : Configuration) (bits : Nat) (rest : List Value)
      (stack : c.operandStack = .f64 bits :: rest) :
      OpStep .i64ReinterpretF64 c
        (.step { c with operandStack := (.i64 (wrap64 (Int.ofNat bits))) :: rest })
  | f32ReinterpretI32 (c : Configuration) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i32 value :: rest) :
      OpStep .f32ReinterpretI32 c
        (.step { c with operandStack := (.f32 (unsignedBits 32 value)) :: rest })
  | f64ReinterpretI64 (c : Configuration) (value : Int) (rest : List Value)
      (stack : c.operandStack = .i64 value :: rest) :
      OpStep .f64ReinterpretI64 c
        (.step { c with operandStack := (.f64 (unsignedBits 64 value)) :: rest })
  | load (c : Configuration) (op : LoadOp) (index : Nat) (arg : MemArg)
      (address : Int) (rest : List Value) (memory : MemoryState) (raw : Nat)
      (stack : c.operandStack = .i32 address :: rest)
      (mem : c.store.memory? index = some memory)
      (read : memory.readLE? (effectiveAddress address arg) (op.width) = some raw) :
      OpStep (.load op index arg) c
        (.step { c with operandStack := (loadedValue op raw) :: rest })
  | loadTrap (c : Configuration) (op : LoadOp) (index : Nat) (arg : MemArg)
      (address : Int) (rest : List Value) (memory : MemoryState)
      (stack : c.operandStack = .i32 address :: rest)
      (mem : c.store.memory? index = some memory)
      (oob : memory.readLE? (effectiveAddress address arg) (op.width) = none) :
      OpStep (.load op index arg) c (.trap .memoryOutOfBounds)
  | store (c : Configuration) (op : StoreOp) (index : Nat) (arg : MemArg)
      (address : Int) (operand : Value) (rest : List Value) (memory updated : MemoryState)
      (stack : c.operandStack = operand :: .i32 address :: rest)
      (mem : c.store.memory? index = some memory)
      (typed : operand.type = op.operandType)
      (write : memory.writeLE? (effectiveAddress address arg) (op.width) (storedBits operand) = some updated) :
      OpStep (.store op index arg) c
        (.step { c with
          operandStack := rest
          store := c.store.setMemory index updated })
  | storeTrap (c : Configuration) (op : StoreOp) (index : Nat) (arg : MemArg)
      (address : Int) (operand : Value) (rest : List Value) (memory : MemoryState)
      (stack : c.operandStack = operand :: .i32 address :: rest)
      (mem : c.store.memory? index = some memory)
      (typed : operand.type = op.operandType)
      (oob : memory.writeLE? (effectiveAddress address arg) (op.width) (storedBits operand) = none) :
      OpStep (.store op index arg) c (.trap .memoryOutOfBounds)
  | memorySize (c : Configuration) (index : Nat) (memory : MemoryState)
      (mem : c.store.memory? index = some memory) :
      OpStep (.memorySize index) c
        (.step (c.push (.i32 (wrap32 (Int.ofNat memory.pages)))))
  | memoryGrow (c : Configuration) (index : Nat) (memory grown : MemoryState)
      (delta : Int) (rest : List Value)
      (stack : c.operandStack = .i32 delta :: rest)
      (mem : c.store.memory? index = some memory)
      (grew : memory.growBy? (unsignedBits 32 delta) = some grown) :
      OpStep (.memoryGrow index) c
        (.step { c with
          operandStack := (.i32 (wrap32 (Int.ofNat memory.pages))) :: rest
          store := c.store.setMemory index grown })
  | memoryGrowRefused (c : Configuration) (index : Nat) (memory : MemoryState)
      (delta : Int) (rest : List Value)
      (stack : c.operandStack = .i32 delta :: rest)
      (mem : c.store.memory? index = some memory)
      (refused : memory.growBy? (unsignedBits 32 delta) = none) :
      OpStep (.memoryGrow index) c
        (.step { c with operandStack := (.i32 (-1)) :: rest })
  | call (c : Configuration) (index : Nat) (function : FunctionState)
      (args rest : List Value)
      (stack : c.operandStack = args ++ rest)
      (depth : c.frames.length < maxCallDepth)
      (fn : c.store.function? index = some function)
      (arity : args.length = function.params.length)
      (typed : args.map Value.type = function.params) :
      OpStep (.call index) c
        (.step (c.pushCall index args rest function.results.length
          (function.localTypes.map defaultValue)))
  | callTrap (c : Configuration) (index : Nat)
      (depth : c.frames.length >= maxCallDepth) :
      OpStep (.call index) c (.trap .callDepthExceeded)
  | callHost (c : Configuration) (capability : String)
      (granted : capability ∈ c.grants) :
      OpStep (.callHost capability) c (.step (c.push (.i32 0)))
  | ret (c : Configuration) (frame : Frame) (results rest : List Value)
      (frameEq : c.activeFrame? = some frame)
      (stack : c.operandStack = results ++ rest)
      (arity : results.length = frame.returnArity)
      (more : c.frames ≠ [frame]) :
      OpStep .ret c
        (.step { c with
          frames := c.frames.dropLast
          controlStack := c.controlStack.take frame.controlBase })
  | retFinal (c : Configuration) (frame : Frame) (results rest : List Value)
      (frameEq : c.activeFrame? = some frame)
      (stack : c.operandStack = results ++ rest)
      (arity : results.length = frame.returnArity)
      (final : c.frames = [frame]) :
      OpStep .ret c (.returning results)
  | endOf (c : Configuration) (frame : Frame) (results rest : List Value)
      (frameEq : c.activeFrame? = some frame)
      (stack : c.operandStack = results ++ rest)
      (arity : results.length = frame.returnArity)
      (more : c.frames ≠ [frame]) :
      OpStep .endInstr c
        (.step { c with
          frames := c.frames.dropLast
          controlStack := c.controlStack.take frame.controlBase })
  | endFinal (c : Configuration) (frame : Frame) (results rest : List Value)
      (frameEq : c.activeFrame? = some frame)
      (stack : c.operandStack = results ++ rest)
      (arity : results.length = frame.returnArity)
      (final : c.frames = [frame]) :
      OpStep .endInstr c (.returning results)

/-- The machine transition: one instruction of the active frame.

The active frame's program counter is advanced before the instruction semantics
is applied, so every rule sees the configuration the next instruction will run
in.
-/
inductive Step : Configuration -> Outcome -> Prop where
  | run {c : Configuration} {out : Outcome} {instr : Instr}
      (fetched : c.currentInstr? = some instr) (rule : OpStep instr (c.advance) out) :
      Step c out

/-- `ret` and `end` share their behavior: pop the frame, keeping its results. -/
def execReturn (c : Configuration) : Option Outcome :=
  match c.activeFrame? with
  | none => none
  | some frame =>
    let arity := frame.returnArity
    let results := c.operandStack.take arity
    if c.operandStack.length >= arity then
      if c.frames = [frame] then some (.returning results)
      else some (.step { c with
        frames := c.frames.dropLast
        controlStack := c.controlStack.take frame.controlBase })
    else none

/-- The executable interpreter for one instruction.

This is a total function returning `none` exactly when no rule of `OpStep`
applies, which is the machine being stuck rather than guessing. It is written
from the same clauses as the rules, and V1 proves the two agree.
-/
def execOp : Instr -> Configuration -> Option Outcome
  | .unreachable, _ => some (Outcome.trap Trap.unreachable)
  | .nop, c => some (.step c)
  | .i32Const v, c => some (.step (c.push (.i32 v)))
  | .i64Const v, c => some (.step (c.push (.i64 v)))
  | .f32Const bits, c => some (.step (c.push (.f32 bits)))
  | .f64Const bits, c => some (.step (c.push (.f64 bits)))
  | .drop, c =>
    match c.operandStack with
    | _ :: rest => some (.step { c with operandStack := rest })
    | [] => none
  | .select ty, c =>
    match c.operandStack with
    | (.i32 cond) :: right :: left :: rest =>
      if left.type = ty && right.type = ty then
        some (.step { c with operandStack := (if cond = 0 then right else left) :: rest })
      else none
    | _ => none
  | .localGet index, c =>
    match c.activeFrame? with
    | none => none
    | some frame => (Store.get? frame.locals index).map fun v => .step (c.push v)
  | .localSet index, c =>
    match c.activeFrame?, c.operandStack with
    | some frame, value :: rest =>
      (Configuration.setLocal? frame index value).map fun f =>
        .step { c with operandStack := rest, frames := c.frames.dropLast ++ [f] }
    | _, _ => none
  | .localTee index, c =>
    match c.activeFrame?, c.operandStack with
    | some frame, value :: _ =>
      (Configuration.setLocal? frame index value).map fun f =>
        .step { c with frames := c.frames.dropLast ++ [f] }
    | _, _ => none
  | .globalGet index, c =>
    match c.store.global? index with
    | none => none
    | some global => some (.step (c.push global.value))
  | .globalSet index, c =>
    match c.store.global? index, c.operandStack with
    | some global, value :: rest =>
      if value.type = global.ty then
        some (.step { c with operandStack := rest, store := c.store.setGlobal index { global with value := value } })
      else none
    | _, _ => none
  | .i32Un op, c =>
    match c.operandStack with
    | .i32 value :: rest =>
      some (.step { c with operandStack := (.i32 (evalI32Un op value)) :: rest })
    | _ => none
  | .i32Bin op, c =>
    match c.operandStack with
    | .i32 right :: .i32 left :: rest =>
      match evalI32Bin op left right with
      | some result => some (.step { c with operandStack := (.i32 result) :: rest })
      | none => some (Outcome.trap Trap.integerDivisionByZero)
    | _ => none
  | .i32Cmp op, c =>
    match c.operandStack with
    | .i32 right :: .i32 left :: rest =>
      some (.step { c with operandStack := (.i32 (evalI32Cmp op left right)) :: rest })
    | _ => none
  | .i64Bin op, c =>
    match c.operandStack with
    | .i64 right :: .i64 left :: rest =>
      match evalI64Bin op left right with
      | some result => some (.step { c with operandStack := (.i64 result) :: rest })
      | none => none
    | _ => none
  | .i64Un op, c =>
    match c.operandStack with
    | .i64 value :: rest =>
      some (.step { c with operandStack := (i64UnValue op value) :: rest })
    | _ => none
  | .i64Cmp op, c =>
    match c.operandStack with
    | .i64 right :: .i64 left :: rest =>
      some (.step { c with operandStack := (.i32 (evalCmpBits 64 op left right)) :: rest })
    | _ => none
  | .i32WrapI64, c =>
    match c.operandStack with
    | .i64 value :: rest =>
      some (.step { c with operandStack := (.i32 (wrap32 value)) :: rest })
    | _ => none
  | .i64ExtendI32S, c =>
    match c.operandStack with
    | .i32 value :: rest =>
      some (.step { c with operandStack := (.i64 (wrap64 value)) :: rest })
    | _ => none
  | .i64ExtendI32U, c =>
    match c.operandStack with
    | .i32 value :: rest =>
      some (.step { c with operandStack := (.i64 (Int.ofNat (unsignedBits 32 value))) :: rest })
    | _ => none
  | .i32ReinterpretF32, c =>
    match c.operandStack with
    | .f32 bits :: rest =>
      some (.step { c with operandStack := (.i32 (wrap32 (Int.ofNat bits))) :: rest })
    | _ => none
  | .i64ReinterpretF64, c =>
    match c.operandStack with
    | .f64 bits :: rest =>
      some (.step { c with operandStack := (.i64 (wrap64 (Int.ofNat bits))) :: rest })
    | _ => none
  | .f32ReinterpretI32, c =>
    match c.operandStack with
    | .i32 value :: rest =>
      some (.step { c with operandStack := (.f32 (unsignedBits 32 value)) :: rest })
    | _ => none
  | .f64ReinterpretI64, c =>
    match c.operandStack with
    | .i64 value :: rest =>
      some (.step { c with operandStack := (.f64 (unsignedBits 64 value)) :: rest })
    | _ => none
  | .load op index arg, c =>
    match c.operandStack, c.store.memory? index with
    | .i32 address :: rest, some memory =>
      match memory.readLE? (effectiveAddress address arg) (op.width) with
      | some raw => some (.step { c with operandStack := (loadedValue op raw) :: rest })
      | none => some (Outcome.trap Trap.memoryOutOfBounds)
    | _, _ => none
  | .store op index arg, c =>
    match c.operandStack, c.store.memory? index with
    | operand :: .i32 address :: rest, some memory =>
      if operand.type = op.operandType then
        match memory.writeLE? (effectiveAddress address arg) (op.width) (storedBits operand) with
        | some updated =>
          some (.step { c with
            operandStack := rest
            store := c.store.setMemory index updated })
        | none => some (Outcome.trap Trap.memoryOutOfBounds)
      else none
    | _, _ => none
  | .memorySize index, c =>
    match c.store.memory? index with
    | none => none
    | some memory => some (.step (c.push (.i32 (wrap32 (Int.ofNat memory.pages)))))
  | .memoryGrow index, c =>
    match c.operandStack, c.store.memory? index with
    | .i32 delta :: rest, some memory =>
      match memory.growBy? (unsignedBits 32 delta) with
      | some grown =>
        some (.step { c with
          operandStack := (.i32 (wrap32 (Int.ofNat memory.pages))) :: rest
          store := c.store.setMemory index grown })
      | none => some (.step { c with operandStack := (.i32 (-1)) :: rest })
    | _, _ => none
  | .call index, c =>
    if c.frames.length >= maxCallDepth then some (Outcome.trap Trap.callDepthExceeded)
    else
      match c.store.function? index with
      | none => none
      | some function =>
        let arity := function.params.length
        let args := c.operandStack.take arity
        let rest := c.operandStack.drop arity
        if args.length = arity && args.map Value.type = function.params then
          some (.step (c.pushCall index args rest function.results.length
            (function.localTypes.map defaultValue)))
        else none
  | .callHost capability, c =>
    if capability ∈ c.grants then some (.step (c.push (.i32 0))) else none
  | .ret, c => execReturn c
  | .endInstr, c => execReturn c

/-- The executable interpreter: fetch the active instruction and run it. -/
def exec (c : Configuration) : Option Outcome :=
  match c.currentInstr? with
  | some instr => execOp instr (c.advance)
  | none => none

end TptWasm
