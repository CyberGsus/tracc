pub mod assembly;
pub mod has_binding;
mod output; // TODO: change output for a better builder (block based, receives IR branching maps for finishing)
use super::allocators::*;
use super::intermediate::*;
use output::AssemblyOutput;

use std::collections::VecDeque;

pub fn codegen_function(function_name: String, mut ir: IR) -> AssemblyOutput {
    let collisions = crate::intermediate::analysis::compute_lifetime_collisions(&ir);
    // TODO: integrate register spill output
    let registers::CodegenHints {
        need_move_to_return_reg,
        save_upon_call,
        mut completely_spilled,
        mut registers,
    } = registers::alloc_registers(
        &ir,
        &collisions,
        analysis::order_by_deps(&ir, collisions.keys().cloned()),
        registers::make_allocator_hints(&ir),
    );

    let alloc_map = memory::make_alloc_map(&ir.code);

    for binding in registers.iter().filter_map(|(binding, reg)| {
        if matches!(reg, assembly::RegisterID::ZeroRegister) {
            Some(*binding)
        } else {
            None
        }
    }) {
        // UNSAFE: the binding is allocated to a read-only register.
        unsafe { crate::intermediate::refactor::remove_binding(&mut ir, binding) };
    }

    alloc_map.keys().cloned().for_each(|allocated_binding| {
        registers.insert(allocated_binding, assembly::RegisterID::StackPointer);
        completely_spilled.remove(&allocated_binding);
    });

    debug_assert!(completely_spilled.is_empty(), "shouldn't have any spills");

    let (memory, mem_size) = memory::figure_out_allocations(&ir, alloc_map, &collisions);

    debug_assert!(save_upon_call.is_empty(), "TODO: implement save upon call");

    debug_assert!(
        need_move_to_return_reg.is_empty(),
        "TODO: implement moves to return register (or generic move to register)"
    );

    // align the stack to 16 bytes
    let mem_size = 16 * ((mem_size as f64 / 16.0f64).ceil() as usize);

    // if mem size is not zero, create an epilogue  block with a label and
    // move all returns to the epilogue block.

    let mut blocks: AssemblyOutput = ir
        .code
        .into_iter()
        .enumerate()
        .flat_map(|(block_index, block)| {
            let mut block = compile_block(block, &memory, &registers);
            block.push_front(assembly::Label::Block { num: block_index });
            block
        })
        .collect();

    // change all blocks to be epilogue
    if mem_size != 0 {
        blocks.iter_mut().for_each(|asm| {
            use assembly::{
                Assembly::Instruction,
                Branch::Unconditional,
                Instruction::{Branch, Ret},
                Label::Epilogue,
            };
            if let Instruction(Ret) = asm {
                *asm = Instruction(Branch(Unconditional {
                    register: None,
                    label: Epilogue,
                }));
            }
        });
        blocks.push_back(assembly::Label::Epilogue);
        blocks.push_back(assembly::Instruction::Add {
            target: assembly::Register::StackPointer,
            lhs: assembly::Register::StackPointer,
            rhs: assembly::Data::Register(assembly::Register::StackPointer),
        });
        blocks.push_back(assembly::Instruction::Ret);
    }

    blocks
        .chain_back(if mem_size == 0 {
            Vec::new()
        } else {
            vec![assembly::Instruction::Sub {
                target: assembly::Register::StackPointer,
                lhs: assembly::Register::StackPointer,
                rhs: assembly::Data::Immediate(mem_size as i32),
            }]
        })
        // declare function as global for linkage
        .cons(assembly::Assembly::Label(function_name.clone()))
        .cons(assembly::Directive::Type(
            function_name.clone(),
            "function".into(),
        ))
        .cons(assembly::Directive::Global(function_name))
}

fn compile_block(
    block: BasicBlock,
    memory: &memory::MemoryMap,
    registers: &registers::RegisterMap,
) -> AssemblyOutput {
    let mut output = AssemblyOutput::new();
    for statement in block.statements {
        output = output.chain(match statement {
            Statement::Assign { index, value } => {
                let register = registers[&index];
                compile_value(value, register, memory, registers)
            }
            Statement::Store {
                mem_binding,
                binding,
                byte_size,
            } => match byte_size {
                ByteSize::U64 => todo!("64-bit stores"),
                ByteSize::U8 => todo!("one byte stores"),
                ByteSize::U32 => assembly::Instruction::Str {
                    register: assembly::Register::from_id(
                        registers[&binding],
                        assembly::BitSize::Bit32,
                    ),
                    address: memory[&mem_binding],
                }
                .into(),
            },
        });
    }
    use assembly::Label;
    match block.end {
        BlockEnd::Branch(branch) => match branch {
            Branch::Unconditional { target } => {
                output = output.chain_one(assembly::Branch::Unconditional {
                    register: None,
                    label: Label::Block { num: target.0 },
                })
            }
            Branch::Conditional {
                flag,
                target_true,
                target_false,
            } => {
                // TODO: map with known already touched CPU flags at the end of every block, and
                // the result they computed, if any.
                output = output
                    .chain_one(assembly::Instruction::Cmp {
                        register: assembly::Register::from_id(
                            registers[&flag],
                            assembly::BitSize::Bit32,
                        ),
                        data: assembly::Data::immediate(0, assembly::BitSize::Bit32),
                    })
                    .chain_one(assembly::Branch::Conditional {
                        condition: assembly::Condition::Equals,
                        label: Label::Block {
                            num: target_false.0,
                        },
                    })
                    .chain_one(assembly::Branch::Unconditional {
                        register: None,
                        label: Label::Block { num: target_true.0 },
                    });
            }
        },
        // TODO: make sure that the returned binding is in the place it should.
        BlockEnd::Return(ret) => output = output.chain_one(assembly::Instruction::Ret),
    }
    output
}

// NOTE: currently the size is always bit32 but there might be a moment in time
// where it's not.
fn could_be_constant_to_data(
    cbc: CouldBeConstant,
    registers: &registers::RegisterMap,
) -> assembly::Data {
    match cbc {
        CouldBeConstant::Binding(binding) => assembly::Data::Register(assembly::Register::from_id(
            registers[&binding],
            assembly::BitSize::Bit32,
        )),
        CouldBeConstant::Constant(constant) => {
            assembly::Data::immediate(constant as i32, assembly::BitSize::Bit32)
        }
    }
}

fn compile_value(
    value: Value,
    target_register: assembly::RegisterID,
    memory: &memory::MemoryMap,
    registers: &registers::RegisterMap,
) -> AssemblyOutput {
    match value {
        // codegen has nothing to do with this.
        Value::Allocate { .. } => AssemblyOutput::new(),
        // codegen won't do anything here with phi nodes. They are analyzed separately
        // and the register allocator is responsible for putting all the bindings in the same place
        Value::Phi { .. } => AssemblyOutput::new(),
        Value::Cmp {
            condition,
            lhs,
            rhs,
        } => AssemblyOutput::from(assembly::Instruction::Cmp {
            register: assembly::Register::from_id(registers[&lhs], assembly::BitSize::Bit32),
            data: could_be_constant_to_data(rhs, registers),
        })
        .chain_one(assembly::Instruction::Cset {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            condition,
        }),
        Value::Load {
            mem_binding,
            byte_size: _, // TODO: use different instruction/register size depending on byte size
        } => assembly::Instruction::Ldr {
            register: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            address: memory[&mem_binding],
        }
        .into(),
        Value::Negate { binding } => assembly::Instruction::Neg {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            source: assembly::Register::from_id(registers[&binding], assembly::BitSize::Bit32),
        }
        .into(),
        // binding XOR FFFFFFFF does the trick.
        Value::FlipBits { binding } => assembly::Instruction::Eor {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            lhs: assembly::Register::from_id(registers[&binding], assembly::BitSize::Bit32),
            rhs: assembly::Data::Immediate(std::i32::MAX),
            bitmask: std::u32::MAX as u64,
        }
        .into(),
        Value::Add { lhs, rhs } => {
            // currently both are only 32 bit
            let lhs_register = registers[&lhs];
            assembly::Instruction::Add {
                target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
                lhs: assembly::Register::from_id(lhs_register, assembly::BitSize::Bit32),
                rhs: could_be_constant_to_data(rhs, registers),
            }
            .into()
        }
        Value::Subtract { lhs, rhs } => assembly::Instruction::Sub {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            lhs: assembly::Register::from_id(registers[&lhs], assembly::BitSize::Bit32),
            rhs: could_be_constant_to_data(rhs, registers),
        }
        .into(),
        Value::Multiply { lhs, rhs } => assembly::Instruction::Mul {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            lhs: assembly::Register::from_id(registers[&lhs], assembly::BitSize::Bit32),
            rhs: could_be_constant_to_data(rhs, registers),
        }
        .into(),
        Value::Divide {
            lhs,
            rhs,
            is_signed,
        } => assembly::Instruction::Div {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            lhs: assembly::Register::from_id(registers[&lhs], assembly::BitSize::Bit32),
            rhs: could_be_constant_to_data(rhs, registers),
            signed: is_signed,
        }
        .into(),
        Value::Lsl { lhs, rhs } => todo!(),
        Value::Lsr { lhs, rhs } => todo!(),
        Value::And { lhs, rhs } => assembly::Instruction::And {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            lhs: assembly::Register::from_id(registers[&lhs], assembly::BitSize::Bit32),
            rhs: could_be_constant_to_data(rhs, registers),
        }
        .into(),
        Value::Or { lhs, rhs } => todo!(),
        Value::Xor { lhs, rhs } => assembly::Instruction::Eor {
            target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
            lhs: assembly::Register::from_id(registers[&lhs], assembly::BitSize::Bit32),
            rhs: could_be_constant_to_data(rhs, registers),
            bitmask: std::u32::MAX as u64,
        }
        .into(),
        Value::Constant(ctant) => {
            assembly::Instruction::Mov {
                target: assembly::Register::from_id(target_register, assembly::BitSize::Bit32),
                source: assembly::Data::immediate(ctant, assembly::BitSize::Bit32),
            }
        }
        .into(),
        Value::Binding(_) => todo!(),
    }
}
