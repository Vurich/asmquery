//! This is a sketch of how a vertical slice through the x86 assembly pipeline might look
//! like.
//!
//! Here's a quick overview/glossary:
//! - Unless otherwise specified, I'll use "instruction" to refer to the actual instructions
//!   on the machine. In the machine spec, multiple instruction definitons could correspond
//!   to a single opcode, as different sets of arguments are classed as different instruction
//!   definitions. Although register-register add and register-immediate add are one
//!   "instruction", they are two separate instruction _definitions_, although almost
//!   certainly we will have abstractions that allow defining a single operation with all of
//!   the forms for register-register, register-immediate, register-memory and so on auto-
//!   generated.
//!
//! - The biggest difference between this and other similar libraries is that the machine
//!   specification is written in terms of operations, which may have a many-to-many
//!   correspondance with actual instructions. Operations are the smallest indivisible piece
//!   of an instruction. An `add` is an operation, but an "`add` and set the carry flag if
//!   it overflows" is two separate operations, as there exist instructions that can add
//!   without setting the carry flag.
//!
//! - To compile a Wasm operation to assembly, we write a low-level infinite register machine
//!   IR that I'm going to call Low IR (just so that we have some name to refer to it that
//!   won't be confused with the actual instructions on the machine). This IR is defined in
//!   terms of _operations_, which take some number of inputs and produce precisely one
//!   output. On most machines, many instructions produce multiple outputs when executed. For
//!   example, the `add` instruction might set the carry flag in the `FLAGS` register. We
//!   would respresent this as two independent Low IR instructions. With an LLVM IR-style
//!   strawman syntax, that might look like this:
//!   ```
//!   %2 = add %0, %1
//!   %3 = add_carry %0, %1
//!   ```
//!   `add_carry` is _not_ add-with-carry, it simply asks whether the add operation with
//!   the provided operands would set the carry bit. On x86, one instruction will do both of
//!   these operations simultaneously, and so we combine them into one. The algorithm that
//!   does this is provided below.
//!
//! - I'll refer to the machine-specific code that defines the Low IR for each Lightbeam IR
//!   instruction as the "Low IR generator", since for our purposes here the only important
//!   thing is that it generates Low IR. The fact that it generates it from Lightbeam IR,
//!   which in turn is generated from WebAssembly, is not particularly important for our
//!   purposes here. Since a large number of operations are valid on every machine - every
//!   machine will have an add, a subtract, a load, a store and so forth - most of this
//!   code can be provided using a default implementation, with relatively few instructions
//!   needing machine-specific implementations.
//!
//! - I'm going to refer to the code that compiles Low IR into actual instructions on the
//!   machine as the Low IR compiler, or LIRC. This code handles allocating real locations
//!   in registers or on the stack for the virtual registers defined in Low IR, and it
//!   handles instruction selection. It's possible for a single instruction to be selected
//!   for many Low IR operations, but impossible for more than one instruction to be
//!   selected for a single Low IR operation. It should also handle control flow, but
//!   precisely how this works is currently unclear. Since the machine specification
//!   provides lots of metadata about the machine, LIRC can be machine-independent,
//!   relying on the Low IR generator to provide Low IR valid for the machine and the
//!   machine specification to provide any metadata needed to do correct register
//!   allocation, instruction selection and control flow.
//!
//! - When I refer to "doing work at compile-time", I mean having that work done while
//!   compiling _Lightbeam_, so that work is already done by the time that Lightbeam
//!   comes to compile some WebAssembly code. I will not use "compile-time" to refer
//!   to Lightbeam compiling WebAssembly, I will say "at runtime" (i.e. when Lightbeam is
//!   running). For the runtime of the code emitted by Lightbeam, I will say words to the
//!   effect of "when we run Lightbeam's output" or "when running the outputted code".
//!
//! -------------------------------------------------------------------------------------
//!
//! The highest-level pseudocode looks like this:
//! * Generate a stateful machine-generic codegen backend `B`, parameterised
//!   with a stateless machine-specific Low IR instruction generator `G`
//! * Generate a stateless machine-specific assembler `A`
//! * For each WebAssembly instruction (call each instruction `WI`)
//!   * Convert `WI` to a stream of Microwasm instructions `MI`
//!   * Convert `MI` into a stream of Machine IR instructions `IRS`
//!     * Each individual Microwasm instruction in `MI` is converted into a stream of
//!       Low IR instructions by `G` and these streams are concatenated together
//!   * Create a stateful cursor `CD` into `IRS`
//!   * While `CD` has not reached the end of `IRS`
//!     * Get the current Low IR instruction `IR` pointed to by `CD` and advance the
//!       cursor
//!     * `B` fills in the virtual registers in `IR` with constraints based on its
//!       internal state to make a query `Q`
//!     * `B` passes `Q` into the assembler to get a list of matches `M`
//!     * `B` checks that there exists some match in `M` that could be emitted (making
//!       sure that e.g. data flow and clobbers are correct), and if not it returns an
//!       error
//!     * Loop
//!       * Get the current Low IR instruction `IR'` pointed to by `CD` and advance the
//!         cursor
//!       * `B` fills in the virtual registers in `IR'` with constraints based on its
//!         internal state to make a query `Q'`
//!       * Refine `M` to only contain the matches that _also_ match `Q'` to create a new
//!         list of matches `M'`
//!       * Exit the loop if `M'` is empty or if there are no matches in `M'` that could be
//!         emitted (w.r.t. data flow, clobbers etc)
//!       * Otherwise, set `M` to be `M'` and repeat the loop
//!     * Get the best match in `M` (by some definition of "best", perhaps by which match
//!       requires the least spilling or even by cycle count)
//!     * `B` fills this match in with specific locations, so precisely one memory location,
//!       register etc., and passes this to the assembler to encode it into machine code and
//!       write it to a byte buffer
//!
//! > NOTE: We can take advantage here of the fact that the list of candidate matches is far
//! >       larger than the list of matches that we could actually emit. In fact, the candidate
//! >       matches that we get from the machine specification are deterministically and
//! >       statelessly generated from each Low IR instruction. This means that we can
//! >       memoise the query result for each Low IR instruction and store it as a bitfield,
//! >       where the "refined" matches that are candidate instructions that may be able to
//! >       produce both outputs can simply be calculated by doing the bitwise `and` of these
//! >       two bitfields. We then iterate through the remaining matches, doing the calculation
//! >       that actually needs to be done at runtime - e.g. data flow and clobbers.
//!
//! The most important thing to note here is that Low IR instrs can only be collapsed so long as
//! there exists some instruction that could collapse each intermediate step. This is because if we
//! read the next instruction and it can't be collapsed into our current state, we can't rewind to
//! find the most-recent set of instructions that _can_ be collapsed - we need to emit something and
//! continue or fail. For example, the complex operations included in x86's memory addressing modes
//! can be collapsed together since at every step we could always just emit a `lea` to do some
//! component of an addressing calculation without doing the load.
//!
//! A cool thing about this algorithm: assuming that it can be efficiently implemented this
//! gives us optimisations like converting add-and-test into add-and-use-side-effect-flags
//! while _also_ converting add-without-test into `lea` where appropriate, without explicitly
//! implementing this as an optimisation or even needing to do any tracking in Lightbeam of
//! what outputs have been produced as a side-effect and which outputs have been clobbered -
//! it's just implicit in the querying algorithm. More optimisations could be implemented
//! with better tracking of clobbering in the generic backend - such as `add, mov, test`
//! being converted into `add, mov` with the test implicit in the `add` but not clobbered by
//! the `mov` - but it's great that we get some optimisations implemented for free. This
//! algorithm will also allow the following:
//!
//! ```
//! %2 = add %0, %1
//! %3 = sub %1, %2
//! %4 = add_carry %0, %1
//! ```
//!
//! Since the `add_carry` output can't be combined with the `sub` output, a query will be performed
//! for `add_carry` alone, which will generate a new add, discard the actual value and keep only
//! the carry bit. Obviously we should avoid generating patterns like this where possible, but it
//! means that if we have an `add` followed by an `add_carry` but DCE eliminates the `add`, we
//! still generate correct code.
//!
//! -------------------------------------------------------------------------------------
//!
//! A machine's complex memory addressing modes can be implemented by expanding
//! the complex series of operations done as part of the memory operation into
//! a series of RISC inputs and outputs. For example, you could define x86's
//! 32-bit add with memory operand by specifying exactly how the memory operand
//! is calculated, splitting it into its component calculations, the load that it
//! performs, and the resulting addition.
//!
//! ```
//! [
//!     // LHS + load(BASE + (INDEX << SCALE) + DISP)
//!     G::Add32.output(int_reg_64, [1, 5]),
//!     // load(BASE + (INDEX << SCALE) + DISP)
//!     G::Load32.output(INTERNAL, [2]),
//!     // BASE + (INDEX << SCALE) + DISP
//!     G::Add32.output(INTERNAL, [3, 4]),
//!     // (INDEX << SCALE) + DISP
//!     G::Add32.output(INTERNAL, [4, 6]),
//!     // INDEX << SCALE
//!     G::ShiftL32.output(INTERNAL, [8, 9]),
//!     input(int_reg_32).eq(0) // LHS operand
//!     input(imm32),           // DISP
//!     input(int_reg_32),      // BASE
//!     input(int_reg_32),      // INDEX
//!     input(imm3),            // SCALE
//! ]
//! ```
//!
//! `INTERNAL` is used as the destination of these intermediate outputs. Precisely
//! how `INTERNAL` is represented isn't important, the important thing is that
//! whatever constraints it defines cannot be fulfilled. This ensures that
//! instructions that do memory operations are considered as candidates to be
//! merged together, but that the merged instruction cannot be emitted if LIRC
//! needs to allocate an actual location for any of these intermediate values,
//! for example if the calculated address is needed later on. Here's an example of
//! what some Low IR with complex memory operations would look like. You'd write
//! Low IR that looks something like the code below and the algorithm above would
//! collapse all of these into a single instruction on x86 but multiple
//! instructions on ARM64 (assume `%base`, `%index` and `%lhs` were defined
//! previously):
//!
//! ```
//! %mem0 = add %base, %disp
//! %shifted_index = shl %index, 2imm3
//! %mem1 = add %mem0, %shifted_index
//! %loaded = load %mem1
//! %added = add %lhs, %loaded
//! ```
//!
//! We'd probably write some helper method for x86 that abstracted this away.
//!
//! The reason that I think that this is a better solution to having some form of
//! explicit memory calculation is that it's a common pattern in generated Wasm code
//! to do some simple calculations followed by a memory access. This is because Wasm
//! only has pretty simple addressing modes. The compiler generating the Wasm code
//! can generally assume that this pattern can be detected and converted into x86
//! addressing instructions. If we split our memory addressing up like this, we
//! can essentially detect and coalesce this pattern for free, whereas if we try to
//! detect and generate it by inspecting the Wasm instructions and generating some
//! form of special memory calculation then we have to thread far more information
//! through the whole of the program. This gives us it for free and keeps most of
//! our code self-contained and stateless.
//!
//! A limitation of this is that if you need to use the same address twice, as far
//! as I can tell it isn't possible to write a single stream of Low IR instructions
//! that would do the address calculation once and save it to an intermediate
//! result on ARM but do the calculation twice on x86, since doing the calculation
//! twice would be slower on ARM but storing to an intermediate register would be
//! slower on x86. Probably we can just delegate that responsibility to the Low IR
//! generator.
//!
//! -------------------------------------------------------------------------------------
//!
//! It might be useful to maintain a bitmask for some subset of outputs that represents
//! the set of instructions that can actually produce that value into a specific
//! register or memory location. For example, `add reg, reg` produces an `Add32` output
//! into location that can be later accessed, whereas after emitting
//! `mov r2, [r0 + r1]` you cannot access `r0 + r1`. We still want to keep the `mov` as
//! a possible candidate in case the next Low IR operation is a `load` so we can't just
//! avoid producing it as a match in the first place. If we have some kind of cache
//! with (probably precalculated) bitmasks based on target locations, we can
//! pretty easily mask out any instructions that we know for sure are going to be
//! invalid and only iterate over the remaining ones. This is especially the case for
//! x86 memory addressing - many, many, many x86 instructions have variants that take a
//! memory operand, so if we query for an "add" or "shift" we'll get a huge number of
//! false positives. We could precache "instructions that can put an add result in any
//! GPR", which will create a bitmask for every instruction is at least one `Add32`
//! output whose set of possible destinations has any overlap at all with the set of
//! GPRs.
//!
//! -------------------------------------------------------------------------------------
//!
//! Clobbers can just be represented as other outputs. A clobber that zeroes some part of
//! a register becomes a `Zero` operation parameterised with the correct size, likewise a
//! clobber that leaves some register in an undefined state could be represented as an
//! `Undefine` operation. The same code that prevents a register from being overwritten by
//! an intended output can also be used to prevent a register from being overwritten by an
//! unintended output. This is one of the biggest places that we have bugs right now so
//! factoring all of the clobber avoidance code into a single place will make codegen far
//! more robust.
//!
//! -------------------------------------------------------------------------------------
//!
//! One thing that is useful to note is that virtual registers should map one-to-one with
//! something that actually exists on the machine. We should never reallocate a register,
//! if it needs to be moved a new register should be allocated. However, we _should_ be
//! able to reallocate anything that was pushed onto the Wasm stack. The internal
//! representation of the stack should be split into a vector of virtual registers and a
//! mapping from virtual register to real location. We may also want a mapping the other
//! way around - from real locaton to virtual register and then from virtual register to
//! positions on the Wasm stack. A mapping that goes the opposite direction means that
//! we can efficiently free up registers without having to iterate over the entire stack.
//!
//! -------------------------------------------------------------------------------------
//!
//! How to model control flow is the biggest question-mark here, as it often is. Although
//! this model works great for straight-line code there are complexities when it comes to
//! modelling any control flow. It might be useful for the methods on the Low IR
//! generator that implement control flow instructions to recieve information like the
//! target calling convention so it can emit `mov`s etc that directly implement this.
//! Having Low IR implement control flow is desirable - I would ideally want to prevent
//! LIRC from generating any Low IR itself whatsoever, even delegating the implementation
//! of `mov`s to the machine-specific generator, but this might not be useful.
//!
//! Something that is cross-purposes with control flow: how do we handle conditional
//! instructions? ARM64 has conditional increment, conditional not, conditional negate,
//! and conditional move, whereas x86 has conditional move and conditional set. We
//! definitely want to at least support conditional move, since there is a Wasm
//! instruction that maps directly to it (`select`).
//!
//! The simplest solution is to just have `CMov` be a separate output, one that takes 3
//! inputs - along with the src and dst we can additionally provide a condition. Ideally,
//! though, we would somehow combine control flow and conditional instructions, since
//! that would mean that we could compile code in Wasm that uses control flow to skips
//! over instructions to use conditional instructions on the target architecture.
//! Perhaps, when hitting control flow where one branch is directly following the current
//! one, we can delay generating the actual branch, only doing so if we hit an instruction
//! that cannot be made conditional. This would end up being pretty hairy though, of
//! course, since we'd have to avoid clobbering any flags etc that the jump would need.
//!
//! -------------------------------------------------------------------------------------
//!
//! An idea for how to handle calling conventions and control flow is as follows: we have
//! a concept of calling conventions in the IR that are defined in terms of virtual
//! registers. Since virtual registers must be globally unique (i.e. you can't redefine
//! them even in distinct codepaths) we can have each block simply define the virtual
//! registers that it needs to be live when you enter it, plus a list of arguments for
//! locations that can be different every time the block is entered. The Low IR would
//! then define a calling convention and apply it to some number of blocks:
//!
//! ```
//! .newcc sharedcc (%bar) [%something]
//!
//!   %something = const i32 1
//!   %condition = is_zero %somereg
//!   %foo = const i32 1
//! .applycc sharedcc (%foo)
//!   jmpif %condition, true_branch
//!   jmp false_branch
//! label true_branch sharedcc:
//!   ;; ...
//! label false_branch sharedcc:
//!   ;; ...
//! ```
//!
//! The registers in the `[]` are dependencies - registers that must be live when the
//! block is entered. There are no restrictions on these registers, for example, they can
//! be constants. Registers in the `()` are arguments - these are passed to the block
//! every time it is called. Since arguments can be different, when the block is first
//! called a mutable location is allocated for it - normally a register. This means that
//! if we pass a constant as an argument we have to spill that constant to a register.
//!
//! This maintains the property that every virtual register must correspond to precisely
//! one location on the machine. For a block that has only one caller, the Low IR
//! generator can create a calling convention that has no arguments.
//!
//! We can also use this system to implement calls, so long as we allow the Low IR to
//! specify arguments as either physical locations or as virtual registers. You could
//! imagine that the Low IR might look something like so:
//!
//! ```
//! .newcc systemvi32_i32 (%rsi, %rdi) []
//! .newcc systemvreti32 (%rax) []
//!
//!   %foo = const i32 0
//!   %bar = const i32 1
//!   %funcpointer = get_function_pointer_somehow
//! .applycc systemvi32_i32 (%foo, %bar)
//!   ;; TODO: Exactly how a call looks isn't clear right now
//!   jmp some_function
//! label return_from_call systemvreti32:
//!   ;; ...
//! ```
//!
//! You can see here that we actually define a new block that would be executed after
//! `some_function` returns. You can see that the fact that `some_function` returns by
//! calling `return_from_call` is implicit, based on the fact that `return_from_call` is
//! directly after the function call. If we wanted to have dead code elimination of any
//! kind we'd have to model this better. The cleanest way to solve this issue would be by
//! splitting the `call` instruction into its components, so a call would be calculating
//! the offset between the `return_from_call` label and the current instruction pointer,
//! push that to stack, and then branch. However, because we only have one-instruction
//! lookahead we can't do this. So probably if we ever want to implement DCE at the
//! level of Low IR we could just have an assembler directive that explicitly marks a
//! label as used.
//!
//! Something we would probably want to do is have a spill instruction that specifies
//! what is off-limits, and then emit a spill instruction for each variable that we want
//! to be maintained across the boundary of the function. For example:
//!
//! ```
//! ;; .. snip..
//! .newcc systemvreti32 (%rax) [%keep_me, %keep_me_too]
//!
//!   ;; ..snip..
//!   %something = const i32 1
//!   %something_else = ;; some calculation that produces its value in `%rsi`
//!   %keep_me = spill %something, [%rsi, %rdi, ..]
//!   %keep_me_too = spill %something_else, [%rsi, %rdi, ..]
//!   ;; ..snip..
//! .applycc systemvi32_i32 (%foo, %bar)
//!   ;; ..snip..
//! label return_from_call systemvreti32:
//!   %new_variable = add %keep_me, %keep_me
//!   ;; ...
//! ```
//!
//! In this case we can see that `%keep_me` would be exactly the same as `%something`
//! because it doesn't overlap with any of the locations in the square brackets, whereas
//! %keep_me_too` would be different to `%something_else`. This same `spill` system can
//! be used when we need a specific register for e.g. `div`, simply emitting a `spill`
//! before we emit the `div`. Although I've written the list of banned locations inline
//! here, this will probably be implemented by having a single register class for each
//! of the kinds of spilling we want to do (systemv calls, `div` instructions, etc) and
//! just referencing them.
//!
//! -------------------------------------------------------------------------------------
//!
//! When we branch or do anything that uses a label, we want to be able to have a
//! location that we can write to with the actual value of that label. Since in the
//! current design, every instruction definition represents precisely one encoding of the
//! instruction, we could have a system where we simply find out how much space we need
//! for the instruction, then we take a note of which instruction definition we need to
//! encode, what the arguments are (not including the ones we'll fill in later) and what
//! our current encoding position is. When that label gets defined, we simply call back
//! into the encoding function of that specific instruction definition and overwrite the
//! whole instruction.
//!
//! To ensure that we don't accidentally write an instruction definition that can return
//! encodings of different sizes depending on the arguments, we could have it so that the
//! method to define a new instruction enforces that the supplied encoding function
//! returns a fixed-size array (probably using a trait). We then don't even need to even
//! call the function when we have relocations, we just get the size and fill it in with
//! zeroes. That way, the assembler doesn't even need to know about the concept of
//! relocations whatsoever.
//!
//! -------------------------------------------------------------------------------------
//!
//! A quick note: everywhere where we use `Vec` we'd ideally use some trickery to do
//! everything on the stack and avoid allocation. Every allocation means work that
//! cannot be done at compile-time and increased difficulty figuring out complexity. I
//! have ideas of precisely how to constrain ourselves to the stack everywhere that we
//! need to be, but to keep this sample code simple I've used `Vec` for now.

#![feature(const_fn, type_alias_impl_trait)]

mod machine;

pub use machine::{
    Action, EncodeArg, EncodeError, EncodeResult, Immediate, InstrBuilder, InstrDef, MachineSpec,
    Param, Reg, RegClass, Var, Variants,
};

pub mod actions {
    pub type Bits = u8;

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub enum Generic {
        Store { input: Bits, mem_size: Bits },
        Load { out: Bits, mem_size: Bits },
        OverflowSigned,
        OverflowUnsigned,
        AddWithCarry(Bits),
        Add(Bits),
        AddWithCarryOverflowS(Bits),
        AddWithCarryOverflowU(Bits),
        AddOverflowS(Bits),
        AddOverflowU(Bits),
        AddFp(Bits),
        And(Bits),
        PackedAnd(Bits),
        ShiftLOverflow(Bits),
        ShiftArithR(Bits), // Arithmetic shift right
        ShiftArithRUnderflowS(Bits),
        ShiftLogicalR(Bits), // Logical shift right
        ShiftLogicalRUnderflowU(Bits),
        DivFp(Bits),
        MaxFp(Bits),
        MinFp(Bits),
        MulFp(Bits),
        SMul(Bits),
        UMul(Bits),
        Or(Bits),
        PackedOr(Bits),
        Xor(Bits),
        PackedXor(Bits),
        ShiftL(Bits),
        SqrtFp(Bits),
        SubWithCarry(Bits),
        Sub(Bits),
        SubWithCarryOverflowS(Bits),
        SubWithCarryOverflowU(Bits),
        SubOverflowS(Bits),
        SubOverflowU(Bits),
        SubFp(Bits),
        Move(Bits),
        IsZero,
        IsNonZero,
        LtZero,
        Clear,
        MulTrunc(Bits), // Result of multiply truncated
        Undefined(Bits),
    }
}

pub mod x64 {
    use crate::actions::{Bits, Generic as G};
    use crate::machine::{Immediate, InstrBuilder, MachineSpec, RegClass, Var};

    pub mod regs {
        crate::regs! {
            pub RAX, RBX, RCX, RDX, RBP, RSI, RDI, RSP, R8, R9, R10, R11, R12, R13, R14, R15,
            CF, OF, ZF, SF, XMM0, XMM1, XMM2, XMM3, XMM4, XMM5, XMM6, XMM7
        }
    }

    pub fn spec() -> MachineSpec<'static, G> {
        trait InstrBuilderExt {
            fn memory(&mut self) -> Var;
            fn arith(&mut self, op: G, overflow_s: G, overflow_u: G, left: Var, right: Var) -> Var;
            fn arith_carry(
                &mut self,
                op: G,
                overflow_s: G,
                overflow_u: G,
                left: Var,
                right: Var,
            ) -> Var;
            fn arith_logical(&mut self, op: G, left: Var, right: Var) -> Var;
            fn arith_fp(&mut self, op: G, left: Var, right: Var) -> Var;
            fn move_action(&mut self, op: G, left: Var, right: Var) -> Var;
            fn integer_smul(
                &mut self,
                op: G,
                size: u8,
                cf_action: G,
                of_action: G,
                left: Var,
                right: Var,
            ) -> Var;
            fn integer_umul(
                &mut self,
                op: G,
                size: u8,
                cf_action: G,
                of_action: G,
                left: Var,
                right: Var,
            ) -> Var;
        }

        trait MachineSpecExt: Sized {
            fn arith_variants<Op, OS, OU, T>(
                self,
                op: Op,
                overflow_s: OS,
                overflow_u: OU,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                OS: FnMut(Bits) -> G,
                OU: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >;

            fn arith_variants_carry<Op, OS, OU, T>(
                self,
                op: Op,
                overflow_s: OS,
                overflow_u: OU,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                OS: FnMut(Bits) -> G,
                OU: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >;
            fn arith_variants_logical<Op, T>(self, op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >;

            fn arith_variants_fp<Op, T>(self, op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str)]>;

            fn arith_variants_shift<Op, Ovf, Cf, T>(
                self,
                op: Op,
                overflow: Ovf,
                carry: Cf,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                Ovf: FnMut(Bits) -> G,
                Cf: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str, &'static str)]>;

            fn signed_multiply_variants<Op, Ovf, Cf, T>(
                self,
                op: Op,
                overflow: Ovf,
                carry: Cf,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                Ovf: FnMut(Bits) -> G,
                Cf: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str, &'static str)]>;

            fn move_transfer_variants<Op, T>(self, op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str, &'static str)]>;

            fn move_packed_variants<Op, T>(self, op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str)]>;

            fn move_variants<Op, T>(self, op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >;
        }

        const MEM_OPERAND_SIZE: Bits = 32;

        impl MachineSpecExt for MachineSpec<'static, G> {
            fn move_variants<Op, T>(mut self, mut op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >,
            {
                for &(size, rr_name, rm_name, mr_name, ri_name, mi_name) in sizes.as_ref() {
                    let op = op(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(INT_REG);

                            let out = new.move_action(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(INT_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.move_action(op, left, right);
                            new.eq(out, left);
                        })
                        .instr(mr_name, |new| {
                            let left_addr = new.memory();
                            let right = new.param(INT_REG);

                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let out = new.move_action(op, left, right);
                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        })
                        .instr(ri_name, |new| {
                            let left = new.param(INT_REG);

                            let right = match size {
                                8 | 16 | 32 => new.param(Immediate { bits: size }),
                                64 => new.param(Immediate { bits: 32 }),
                                _ => panic!("move_variants: Bad immediate size"),
                            };
                            let out = new.move_action(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(mi_name, |new| {
                            let left_addr = new.memory();
                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let right = match size {
                                8 | 16 | 32 => new.param(Immediate { bits: size }),
                                64 => new.param(Immediate { bits: 32 }),
                                _ => panic!("move_variants: Bad immediate size"),
                            };

                            let out = new.move_action(op, left, right);

                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        });
                }

                self
            }

            fn arith_variants<Op, OS, OU, T>(
                mut self,
                mut op: Op,
                mut overflow_s: OS,
                mut overflow_u: OU,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                OS: FnMut(Bits) -> G,
                OU: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >,
            {
                for &(size, rr_name, rm_name, mr_name, ri_name, mi_name) in sizes.as_ref() {
                    let op = op(size);
                    let overflow_s = overflow_s(size);
                    let overflow_u = overflow_u(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(INT_REG);

                            let out = new.arith(op, overflow_s, overflow_u, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(INT_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.arith(op, overflow_s, overflow_u, left, right);
                            new.eq(out, left);
                        })
                        .instr(mr_name, |new| {
                            let left_addr = new.memory();
                            let right = new.param(INT_REG);

                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let out = new.arith(op, overflow_s, overflow_u, left, right);
                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        })
                        .instr(ri_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(Immediate { bits: 32 });

                            let out = new.arith(op, overflow_s, overflow_u, left, right);
                            new.eq(left, out);
                        })
                        .instr(mi_name, |new| {
                            let left_addr = new.memory();
                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let right = new.param(Immediate { bits: 32 });
                            let out = new.arith(op, overflow_s, overflow_u, left, right);

                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        });
                }

                self
            }

            fn arith_variants_carry<Op, OS, OU, T>(
                mut self,
                mut op: Op,
                mut overflow_s: OS,
                mut overflow_u: OU,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                OS: FnMut(Bits) -> G,
                OU: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >,
            {
                for &(size, rr_name, rm_name, mr_name, ri_name, mi_name) in sizes.as_ref() {
                    let op = op(size);
                    let overflow_s = overflow_s(size);
                    let overflow_u = overflow_u(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(INT_REG);

                            let out = new.arith_carry(op, overflow_s, overflow_u, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(INT_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.arith_carry(op, overflow_s, overflow_u, left, right);
                            new.eq(out, left);
                        })
                        .instr(mr_name, |new| {
                            let left_addr = new.memory();
                            let right = new.param(INT_REG);

                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let out = new.arith_carry(op, overflow_s, overflow_u, left, right);
                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        })
                        .instr(ri_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(Immediate { bits: 32 });

                            let out = new.arith_carry(op, overflow_s, overflow_u, left, right);
                            new.eq(left, out);
                        })
                        .instr(mi_name, |new| {
                            let left_addr = new.memory();
                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let right = new.param(Immediate { bits: 32 });
                            let out = new.arith_carry(op, overflow_s, overflow_u, left, right);

                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        });
                }

                self
            }

            fn arith_variants_logical<Op, T>(mut self, mut op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<
                    [(
                        Bits,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                        &'static str,
                    )],
                >,
            {
                for &(size, rr_name, rm_name, mr_name, ri_name, mi_name) in sizes.as_ref() {
                    let op = op(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(INT_REG);

                            let out = new.arith_logical(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(INT_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.arith_logical(op, left, right);
                            new.eq(out, left);
                        })
                        .instr(mr_name, |new| {
                            let left_addr = new.memory();
                            let right = new.param(INT_REG);

                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let out = new.arith_logical(op, left, right);
                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        })
                        .instr(ri_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(Immediate { bits: 32 });

                            let out = new.arith_logical(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(mi_name, |new| {
                            let left_addr = new.memory();
                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let right = new.param(Immediate { bits: 32 });
                            let out = new.arith_logical(op, left, right);

                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        });
                }

                self
            }

            fn arith_variants_fp<Op, T>(mut self, mut op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str)]>,
            {
                for &(size, rr_name, rm_name) in sizes.as_ref() {
                    let op = op(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(FP_REG);
                            let right = new.param(FP_REG);

                            let out = new.arith_fp(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(FP_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.arith_fp(op, left, right);
                            new.eq(out, left);
                        });
                }

                self
            }

            fn move_packed_variants<Op, T>(mut self, mut op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str)]>,
            {
                for &(size, rr_name, rm_name, mr_name) in sizes.as_ref() {
                    let op = op(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(FP_REG);
                            let right = new.param(FP_REG);

                            let out = new.move_action(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(FP_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.move_action(op, left, right);
                            new.eq(out, left);
                        })
                        .instr(mr_name, |new| {
                            let left_addr = new.memory();
                            let right = new.param(FP_REG);

                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let out = new.move_action(op, left, right);
                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        });
                }

                self
            }

            fn move_transfer_variants<Op, T>(mut self, mut op: Op, sizes: T) -> Self
            where
                Op: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str, &'static str)]>,
            {
                for &(size, mm_r_name, mm_mem_name, r_mm_name, mem_mm_name) in sizes.as_ref() {
                    let op = op(size);

                    self = self
                        .instr(mm_r_name, |new| {
                            let left = new.param(FP_REG);
                            let right = new.param(INT_REG);

                            let out = new.move_action(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(mm_mem_name, |new| {
                            let left = new.param(FP_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.move_action(op, left, right);
                            new.eq(out, left);
                        })
                        .instr(r_mm_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(FP_REG);

                            let out = new.move_action(op, left, right);
                            new.eq(left, out);
                        })
                        .instr(mem_mm_name, |new| {
                            let left = new.param(FP_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out = new.move_action(op, left, right);
                            new.eq(left, out);
                        });
                }

                self
            }

            fn signed_multiply_variants<Op, Ovf, Cf, T>(
                mut self,
                mut op: Op,
                mut overflow: Ovf,
                mut carry: Cf,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                Ovf: FnMut(Bits) -> G,
                Cf: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str, &'static str)]>,
            {
                for &(size, rr_name, rm_name, ri_name, mi_name) in sizes.as_ref() {
                    let op = op(size);
                    let smul_overflow = overflow(size);
                    let smul_carry = carry(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(INT_REG);

                            let out =
                                new.integer_smul(op, size, smul_overflow, smul_carry, left, right);
                            new.eq(left, out);
                        })
                        .instr(rm_name, |new| {
                            let left = new.param(INT_REG);
                            let right_addr = new.memory();

                            let right = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [right_addr],
                            );

                            let out =
                                new.integer_smul(op, size, smul_overflow, smul_carry, left, right);
                            new.eq(out, left);
                        })
                        .instr(ri_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(Immediate { bits: 32 });

                            // Note in this form (imul rn, rn, imm32 the destination
                            // register does not have to equal the first source operand
                            let _out =
                                new.integer_smul(op, size, smul_overflow, smul_carry, left, right);
                        })
                        .instr(mi_name, |new| {
                            let left_addr = new.memory();
                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            // Note in this form (imul rn, mn, imm32 the destination
                            // register does not have to equal the first source operand

                            let right = new.param(Immediate { bits: 32 });
                            let _out =
                                new.integer_smul(op, size, smul_overflow, smul_carry, left, right);
                        });
                }
                self
            }

            fn arith_variants_shift<Op, Ovf, Cf, T>(
                mut self,
                mut op: Op,
                mut overflow: Ovf,
                mut carry: Cf,
                sizes: T,
            ) -> Self
            where
                Op: FnMut(Bits) -> G,
                Ovf: FnMut(Bits) -> G,
                Cf: FnMut(Bits) -> G,
                T: AsRef<[(Bits, &'static str, &'static str, &'static str, &'static str)]>,
            {
                for &(size, rr_name, mr_name, ri_name, mi_name) in sizes.as_ref() {
                    let op = op(size);
                    let shift_overflow = overflow(size);
                    let shift_carry = carry(size);

                    self = self
                        .instr(rr_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(&regs::RCX);

                            let out = new.arith(op, shift_overflow, shift_carry, left, right);
                            new.eq(left, out);
                        })
                        .instr(mr_name, |new| {
                            let left_addr = new.memory();
                            let right = new.param(&regs::RCX);

                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let out = new.arith(op, shift_overflow, shift_carry, left, right);
                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        })
                        .instr(ri_name, |new| {
                            let left = new.param(INT_REG);
                            let right = new.param(Immediate { bits: 8 });

                            let out = new.arith(op, shift_overflow, shift_carry, left, right);
                            new.eq(left, out);
                        })
                        .instr(mi_name, |new| {
                            let left_addr = new.memory();
                            let left = new.action(
                                G::Load {
                                    out: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [left_addr],
                            );

                            let right = new.param(Immediate { bits: 8 });
                            let out = new.arith(op, shift_overflow, shift_carry, left, right);

                            let _ = new.action(
                                G::Store {
                                    input: size,
                                    mem_size: MEM_OPERAND_SIZE,
                                },
                                [out],
                            );
                        });
                }
                self
            }
        }

        impl InstrBuilderExt for InstrBuilder<'_, G> {
            fn memory(&mut self) -> Var {
                self.variants::<typenum::consts::U1>()
                    .or(|[out], new| {
                        let address = new.param(INT_REG);
                        new.eq(out, address);
                    })
                    .or(|[out], new| {
                        let base = new.param(INT_REG);
                        let index = new.param(INT_REG);
                        new.action_into(out, G::Add(MEM_OPERAND_SIZE), vec![base, index]);
                    })
                    .or(|[out], new| {
                        let base = new.param(INT_REG);
                        let disp = new.param(Immediate {
                            bits: MEM_OPERAND_SIZE,
                        });
                        new.action_into(out, G::Add(MEM_OPERAND_SIZE), vec![base, disp]);
                    })
                    .or(|[out], new| {
                        let base = new.param(INT_REG);
                        let index = new.param(INT_REG);
                        let disp = new.param(Immediate {
                            bits: MEM_OPERAND_SIZE,
                        });
                        let intermediate = new.action(G::Add(MEM_OPERAND_SIZE), vec![base, index]);
                        new.action_into(out, G::Add(MEM_OPERAND_SIZE), vec![intermediate, disp]);
                    })
                    .or(|[out], new| {
                        let base = new.param(INT_REG);

                        let index = new.param(INT_REG);
                        let scale = new.param(Immediate { bits: 3 });
                        let shifted_index =
                            new.action(G::ShiftL(MEM_OPERAND_SIZE), vec![index, scale]);

                        let disp = new.param(Immediate {
                            bits: MEM_OPERAND_SIZE,
                        });
                        let intermediate =
                            new.action(G::Add(MEM_OPERAND_SIZE), vec![base, shifted_index]);
                        new.action_into(out, G::Add(MEM_OPERAND_SIZE), vec![intermediate, disp]);
                    })
                    .finish()[0]
            }

            fn integer_smul(
                &mut self,
                op: G,
                size: u8,
                cf_action: G,
                of_action: G,
                left: Var,
                right: Var,
            ) -> Var {
                let out = self.action(op, [left, right]);
                self.action_into(&regs::CF, cf_action, [out]);
                self.action_into(&regs::OF, of_action, [out]);
                self.action_into(&regs::ZF, G::Undefined(size), [out]);
                self.action_into(&regs::SF, G::Undefined(size), [out]);

                out
            }

            fn integer_umul(
                &mut self,
                op: G,
                size: u8,
                cf_action: G,
                of_action: G,
                left: Var,
                right: Var,
            ) -> Var {
                let out = self.action(op, [left, right]);
                let dest = self.param(&regs::RAX);
                self.eq(dest, out);

                self.action_into(&regs::CF, cf_action, [out]);
                self.action_into(&regs::OF, of_action, [out]);
                self.action_into(&regs::ZF, G::Undefined(size), [out]);
                self.action_into(&regs::SF, G::Undefined(size), [out]);
                self.action_into(&regs::RDX, G::Undefined(size), [out]);

                out
            }

            fn arith(&mut self, op: G, overflow_s: G, overflow_u: G, left: Var, right: Var) -> Var {
                let out = self.action(op, [left, right]);
                self.action_into(&regs::CF, overflow_u, [out]);
                self.action_into(&regs::OF, overflow_s, [out]);
                self.action_into(&regs::ZF, G::IsZero, [out]);
                self.action_into(&regs::SF, G::LtZero, [out]);

                out
            }

            fn arith_carry(
                &mut self,
                op: G,
                overflow_s: G,
                overflow_u: G,
                left: Var,
                right: Var,
            ) -> Var {
                let carry = self.param(&regs::CF);
                let out = self.action(op, [left, right, carry]);
                self.action_into(&regs::CF, overflow_u, [out]);
                self.action_into(&regs::OF, overflow_s, [out]);
                self.action_into(&regs::ZF, G::IsZero, [out]);
                self.action_into(&regs::SF, G::LtZero, [out]);

                out
            }

            fn arith_logical(&mut self, op: G, left: Var, right: Var) -> Var {
                let out = self.action(op, [left, right]);

                self.action_into(&regs::CF, G::Clear, []);
                self.action_into(&regs::OF, G::Clear, []);
                self.action_into(&regs::ZF, G::IsZero, [out]);
                self.action_into(&regs::SF, G::LtZero, [out]);

                out
            }

            fn arith_fp(&mut self, op: G, left: Var, right: Var) -> Var {
                let out = self.action(op, [left, right]);

                out
            }

            fn move_action(&mut self, op: G, left: Var, right: Var) -> Var {
                let out = self.action(op, [left, right]);

                out
            }
        }

        // When we define `R0` etc, we should specify its size in bits
        // We _don't_ specify masks here - registers as defined at this point must be non-overlapping,
        // with masking and overlapping semantics defined at the level of the instructions.
        const INT_REG: RegClass = RegClass(&[
            regs::RAX,
            regs::RBX,
            regs::RCX,
            regs::RDX,
            regs::RBP,
            regs::RSI,
            regs::RDI,
            regs::RSP,
            regs::R9,
            regs::R10,
            regs::R11,
            regs::R12,
            regs::R13,
            regs::R14,
            regs::R15,
        ]);
        const FP_REG: RegClass = RegClass(&[
            regs::XMM0,
            regs::XMM1,
            regs::XMM2,
            regs::XMM3,
            regs::XMM4,
            regs::XMM5,
            regs::XMM6,
            regs::XMM7,
        ]);

        MachineSpec::new()
            .arith_variants(
                G::Add,
                G::AddOverflowS,
                G::AddOverflowU,
                [
                    (
                        32,
                        "add r32, r32",
                        "add r32, m32",
                        "add m32, r32",
                        "add r32, i32",
                        "add m32, i32",
                    ),
                    (
                        64,
                        "add r64, r64",
                        "add r64, m64",
                        "add m64, r64",
                        "add r64, i32",
                        "add m64, i32",
                    ),
                ],
            )
            .arith_variants_carry(
                G::AddWithCarry,
                G::AddWithCarryOverflowS,
                G::AddWithCarryOverflowU,
                [
                    (
                        32,
                        "adc r32, r32",
                        "adc r32, m32",
                        "adc m32, r32",
                        "adc r32, i32",
                        "adc m32, i32",
                    ),
                    (
                        64,
                        "adc r64, r64",
                        "adc r64, m64",
                        "adc m64, r64",
                        "adc r64, i32",
                        "adc m64, i32",
                    ),
                ],
            )
            .arith_variants_fp(
                G::AddFp,
                [
                    (32, "addss r32, r32", "addss r32, m32"),
                    (64, "addsd r64, r64", "addsd r64, m64"),
                ],
            )
            .arith_variants_logical(
                G::And,
                [
                    (
                        32,
                        "and r32, r32",
                        "and r32, m32",
                        "and m32, r32",
                        "and r32, i32",
                        "and m32, i32",
                    ),
                    (
                        64,
                        "and r64, r64",
                        "and r64, m64",
                        "and m64, r64",
                        "and r64, i32",
                        "and m64, i32",
                    ),
                ],
            )
            .arith_variants_fp(
                G::PackedAnd,
                [
                    (32, "andps r128, r128", "andps r128, m128"),
                    (64, "andpd r128, r128", "andpd r128, m128"),
                ],
            )
            .arith_variants_fp(
                G::PackedOr,
                [
                    (32, "orps r128, r128", "orps r128, m128"),
                    (64, "orpd r128, r128", "orpd r128, m128"),
                ],
            )
            .arith_variants_fp(
                G::PackedXor,
                [
                    (32, "xorps r128, r128", "xorps r128, m128"),
                    (64, "xorpd r128, r128", "xorpd r128, m128"),
                ],
            )
            .arith_variants_fp(
                G::DivFp,
                [
                    (32, "divss r32, r32", "divss r32, m32"),
                    (64, "divsd r64, r64", "divsd r64, m64"),
                ],
            )
            .arith_variants_fp(
                G::MaxFp,
                [
                    (32, "maxss r32, r32", "maxss r32, m32"),
                    (64, "maxsd r64, r64", "maxsd r64, m64"),
                ],
            )
            .arith_variants_fp(
                G::MinFp,
                [
                    (32, "minss r32, r32", "minss r32, m32"),
                    (64, "minsd r64, r64", "minsd r64, m64"),
                ],
            )
            .arith_variants_fp(
                G::MulFp,
                [
                    (32, "mulss r32, r32", "mulss r32, m32"),
                    (64, "mulsd r64, r64", "mulsd r64, m64"),
                ],
            )
            .arith_variants_fp(
                G::SqrtFp,
                [
                    (32, "sqrtss r32, r32", "sqrtss r32, m32"),
                    (64, "sqrtsd r64, r64", "sqrtsd r64, m64"),
                ],
            )
            .arith_variants_logical(
                G::Or,
                [
                    (
                        32,
                        "or r32, r32",
                        "or r32, m32",
                        "or m32, r32",
                        "or r32, i32",
                        "or m32, i32",
                    ),
                    (
                        64,
                        "or r64, r64",
                        "or r64, m64",
                        "or m64, r64",
                        "or r64, i32",
                        "or m64, i32",
                    ),
                ],
            )
            .arith_variants_logical(
                G::Xor,
                [
                    (
                        32,
                        "xor r32, r32",
                        "xor r32, m32",
                        "xor m32, r32",
                        "xor r32, i32",
                        "xor m32, i32",
                    ),
                    (
                        64,
                        "xor r64, r64",
                        "xor r64, m64",
                        "xor m64, r64",
                        "xor r64, i32",
                        "xor m64, i32",
                    ),
                ],
            )
            .arith_variants(
                G::Sub,
                G::SubOverflowS,
                G::SubOverflowU,
                [
                    (
                        32,
                        "sub r32, r32",
                        "sub r32, m32",
                        "sub m32, r32",
                        "sub r32, i32",
                        "sub m32, i32",
                    ),
                    (
                        64,
                        "sub r64, r64",
                        "sub r64, m64",
                        "sub m64, r64",
                        "sub r64, i32",
                        "sub m64, i32",
                    ),
                ],
            )
            .arith_variants_carry(
                G::SubWithCarry,
                G::SubWithCarryOverflowS,
                G::SubWithCarryOverflowU,
                [
                    (
                        32,
                        "sbb r32, r32",
                        "sbb r32, m32",
                        "sbb m32, r32",
                        "sbb r32, i32",
                        "sbb m32, i32",
                    ),
                    (
                        64,
                        "sbb r64, r64",
                        "sbb r64, m64",
                        "sbb m64, r64",
                        "sbb r64, i32",
                        "sbb m64, i32",
                    ),
                ],
            )
            .arith_variants_fp(
                G::SubFp,
                [
                    (32, "subss r32, r32", "subss r32, m32"),
                    (64, "subsd r64, r64", "subsd r64, m64"),
                ],
            )
            .arith_variants_shift(
                G::ShiftArithR,
                G::Undefined,
                G::ShiftArithRUnderflowS,
                [
                    (
                        32,
                        "sar r32, cl",
                        "sar m32, cl",
                        "sar r32, i8",
                        "sar m32, i8",
                    ),
                    (
                        64,
                        "sar r64, cl",
                        "sar m64, cl",
                        "sar r64, i8",
                        "sar m64, i8",
                    ),
                ],
            )
            .arith_variants_shift(
                G::ShiftL,
                G::Undefined,
                G::ShiftLOverflow,
                [
                    (
                        32,
                        "shl r32, cl",
                        "shl m32, cl",
                        "shl r32, i8",
                        "shl m32, i8",
                    ),
                    (
                        64,
                        "shl r64, cl",
                        "shl m64, cl",
                        "shl r64, i8",
                        "shl m64, i8",
                    ),
                ],
            )
            .arith_variants_shift(
                G::ShiftLogicalR,
                G::Undefined,
                G::ShiftLogicalRUnderflowU,
                [
                    (
                        32,
                        "shr r32, cl",
                        "shr m32, cl",
                        "shr r32, i8",
                        "shr m32, i8",
                    ),
                    (
                        64,
                        "shr r64, cl",
                        "shr m64, cl",
                        "shr r64, i8",
                        "shr m64, i8",
                    ),
                ],
            )
            .signed_multiply_variants(
                G::SMul,
                G::MulTrunc,
                G::MulTrunc,
                [
                    (
                        32,
                        "imul r32, r32",
                        "imul r32, m32",
                        "imul r32, r32, imm32",
                        "imul r32, m32, imm32",
                    ),
                    (
                        64,
                        "imul r64, r64",
                        "imul r64, m64",
                        "imul r64, r64, imm32",
                        "imul r64, m64, imm32",
                    ),
                ],
            )
            .move_variants(
                G::Move,
                [
                    (
                        8,
                        "mov r8, r8",
                        "mov r8, m8",
                        "mov m8, r8",
                        "mov r8, i8",
                        "mov m8, i8",
                    ),
                    (
                        16,
                        "mov r16, r16",
                        "mov r16, m16",
                        "mov m16, r16",
                        "mov r16, i16",
                        "mov m16, i16",
                    ),
                    (
                        32,
                        "mov r32, r32",
                        "mov r32, m32",
                        "mov m32, r32",
                        "mov r32, i32",
                        "mov m32, i32",
                    ),
                    (
                        64,
                        "mov r64, r64",
                        "mov r64, m64",
                        "mov m64, r64",
                        "mov r64, i32",
                        "mov m64, i32",
                    ),
                ],
            )
            .move_transfer_variants(
                G::Move,
                [
                    (
                        32,
                        "movd f32, r32",
                        "movd f32, m32",
                        "movd r32, f32",
                        "movd m32, f32",
                    ),
                    (
                        64,
                        "movq f64, r64",
                        "movq f64, m64",
                        "movq r64, f64",
                        "movq m64, f64",
                    ),
                ],
            )
            .move_packed_variants(
                G::Move,
                [
                    (
                        32,
                        "movaps f128, f128",
                        "movaps f128, m128",
                        "movaps m128, f128",
                    ),
                    (
                        64,
                        "movapd f128, f128",
                        "movapd f128, m128",
                        "movapd m128, f128",
                    ),
                ],
            )
            .instr("mul r32", |new| {
                let left = new.param(INT_REG);
                let right = new.param(INT_REG);

                let _ = new.integer_umul(G::UMul(32), 32, G::IsNonZero, G::IsNonZero, left, right);
            })
            .instr("mul m32", |new| {
                let left = new.param(INT_REG);
                let right_addr = new.memory();
                let right = new.action(
                    G::Load {
                        out: 32,
                        mem_size: MEM_OPERAND_SIZE,
                    },
                    [right_addr],
                );

                let _ = new.integer_umul(G::UMul(32), 32, G::IsNonZero, G::IsNonZero, left, right);
            })
            .instr("mul r64", |new| {
                let left = new.param(INT_REG);
                let right = new.param(INT_REG);

                let _ = new.integer_umul(G::UMul(64), 64, G::IsNonZero, G::IsNonZero, left, right);
            })
            .instr("mul m64", |new| {
                let left = new.param(INT_REG);
                let right_addr = new.memory();
                let right = new.action(
                    G::Load {
                        out: 64,
                        mem_size: MEM_OPERAND_SIZE,
                    },
                    [right_addr],
                );

                let _ = new.integer_umul(G::UMul(64), 64, G::IsNonZero, G::IsNonZero, left, right);
            })
            .instr("cmp r32, r32", |new| {
                let left = new.param(INT_REG);
                let right = new.param(INT_REG);

                let _ = new.arith(
                    G::Sub(32),
                    G::SubOverflowS(32),
                    G::SubOverflowU(32),
                    left,
                    right,
                );
            })
            .instr("cmp r32, m32", |new| {
                let left = new.param(INT_REG);
                let right_addr = new.memory();
                let right = new.action(
                    G::Load {
                        out: 32,
                        mem_size: MEM_OPERAND_SIZE,
                    },
                    [right_addr],
                );

                let _ = new.arith(
                    G::Sub(32),
                    G::SubOverflowS(32),
                    G::SubOverflowU(32),
                    left,
                    right,
                );
            })
            .instr("cmp m32, r32", |new| {
                let left_addr = new.memory();
                let left = new.action(
                    G::Load {
                        out: 32,
                        mem_size: MEM_OPERAND_SIZE,
                    },
                    [left_addr],
                );
                let right = new.param(INT_REG);

                let _ = new.arith(
                    G::Sub(32),
                    G::SubOverflowS(32),
                    G::SubOverflowU(32),
                    left,
                    right,
                );
            })
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn x64_is_correct() {
        panic!("{}", crate::x64::spec());
    }
}
