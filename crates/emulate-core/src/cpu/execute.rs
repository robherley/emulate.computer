//! Integer/system/atomic execution. FP ops are delegated to [`super::fpu`].
//!
//! Conventions:
//! - `cpu.next_pc` is already `pc + len`; jumps/branches overwrite it.
//!   `sret`/`mret` also set it. `cpu.pc` must not be modified here.
//! - Register writes go through `cpu.set_reg` (x0 protected).
//! - Any fault returns `Err(Exception)`; the caller handles trap entry, and
//!   in that case `next_pc` is discarded (pc stays at the faulting instr).
//! - `raw` is the raw instruction (for IllegalInstruction tval).

use super::bitmanip;
use super::csr::{self, mstatus};
use super::decode::*;
use super::{fpu, Cpu, Mode};
use crate::bus::Bus;
use crate::trap::Exception;

/// 64-bit ALU (I + M). Register-form shift amounts are masked here (& 0x3F);
/// immediate forms already carry a 6-bit shamt, so the mask is a no-op.
#[expect(
    clippy::manual_checked_ops,
    reason = "RISC-V defines division-by-zero and signed-overflow results explicitly"
)]
fn alu64(kind: AluOp, a: u64, b: u64) -> u64 {
    match kind {
        AluOp::Add => a.wrapping_add(b),
        AluOp::Sub => a.wrapping_sub(b),
        AluOp::Sll => a << (b & 0x3F),
        AluOp::Slt => ((a as i64) < (b as i64)) as u64,
        AluOp::Sltu => (a < b) as u64,
        AluOp::Xor => a ^ b,
        AluOp::Srl => a >> (b & 0x3F),
        AluOp::Sra => ((a as i64) >> (b & 0x3F)) as u64,
        AluOp::Or => a | b,
        AluOp::And => a & b,
        AluOp::Mul => a.wrapping_mul(b),
        AluOp::Mulh => (((a as i64 as i128).wrapping_mul(b as i64 as i128)) >> 64) as u64,
        AluOp::Mulhsu => (((a as i64 as i128).wrapping_mul(b as i128)) >> 64) as u64,
        AluOp::Mulhu => (((a as u128).wrapping_mul(b as u128)) >> 64) as u64,
        AluOp::Div => {
            let (a, b) = (a as i64, b as i64);
            if b == 0 {
                u64::MAX // quotient all-ones (-1)
            } else if a == i64::MIN && b == -1 {
                i64::MIN as u64
            } else {
                a.wrapping_div(b) as u64
            }
        }
        AluOp::Divu => {
            if b == 0 {
                u64::MAX
            } else {
                a / b
            }
        }
        AluOp::Rem => {
            let (a, b) = (a as i64, b as i64);
            if b == 0 {
                a as u64 // remainder = dividend
            } else if a == i64::MIN && b == -1 {
                0
            } else {
                a.wrapping_rem(b) as u64
            }
        }
        AluOp::Remu => {
            if b == 0 {
                a
            } else {
                a % b
            }
        }
    }
}

/// 32-bit (`*W`) ALU: compute on the low 32 bits, sign-extend the 32-bit
/// result to 64. Shift amounts are masked to 5 bits.
#[expect(
    clippy::manual_checked_ops,
    reason = "RISC-V defines division-by-zero and signed-overflow results explicitly"
)]
fn alu32(kind: AluOp, a: u64, b: u64) -> u64 {
    let au = a as u32;
    let bu = b as u32;
    let ai = au as i32;
    let bi = bu as i32;
    let sh = (b & 0x1F) as u32;
    let res: i32 = match kind {
        AluOp::Add => au.wrapping_add(bu) as i32,
        AluOp::Sub => au.wrapping_sub(bu) as i32,
        AluOp::Sll => (au << sh) as i32,
        AluOp::Srl => (au >> sh) as i32,
        AluOp::Sra => ai >> sh,
        AluOp::Mul => ai.wrapping_mul(bi),
        AluOp::Div => {
            if bi == 0 {
                -1
            } else if ai == i32::MIN && bi == -1 {
                i32::MIN
            } else {
                ai.wrapping_div(bi)
            }
        }
        AluOp::Divu => {
            if bu == 0 {
                u32::MAX as i32
            } else {
                (au / bu) as i32
            }
        }
        AluOp::Rem => {
            if bi == 0 {
                ai
            } else if ai == i32::MIN && bi == -1 {
                0
            } else {
                ai.wrapping_rem(bi)
            }
        }
        AluOp::Remu => {
            if bu == 0 {
                ai
            } else {
                (au % bu) as i32
            }
        }
        // Not encodable as *W forms; fall back to the 64-bit op truncated.
        _ => alu64(kind, a, b) as i32,
    };
    res as i64 as u64
}

#[inline]
fn amo_size(width: AmoWidth) -> u8 {
    match width {
        AmoWidth::W => 4,
        AmoWidth::D => 8,
    }
}

/// Compute the new memory value for an AMO. `old` and `src` are the raw
/// operand values at the operation width (low bits significant for W).
fn amo_result(op: AmoOp, width: AmoWidth, old: u64, src: u64) -> u64 {
    match width {
        AmoWidth::D => match op {
            AmoOp::Swap => src,
            AmoOp::Add => old.wrapping_add(src),
            AmoOp::Xor => old ^ src,
            AmoOp::And => old & src,
            AmoOp::Or => old | src,
            AmoOp::Min => (old as i64).min(src as i64) as u64,
            AmoOp::Max => (old as i64).max(src as i64) as u64,
            AmoOp::Minu => old.min(src),
            AmoOp::Maxu => old.max(src),
        },
        AmoWidth::W => {
            let a = old as u32;
            let b = src as u32;
            let r: u32 = match op {
                AmoOp::Swap => b,
                AmoOp::Add => a.wrapping_add(b),
                AmoOp::Xor => a ^ b,
                AmoOp::And => a & b,
                AmoOp::Or => a | b,
                AmoOp::Min => (a as i32).min(b as i32) as u32,
                AmoOp::Max => (a as i32).max(b as i32) as u32,
                AmoOp::Minu => a.min(b),
                AmoOp::Maxu => a.max(b),
            };
            r as u64
        }
    }
}

pub fn execute(cpu: &mut Cpu, bus: &mut Bus, raw: u32, op: &Op) -> Result<(), Exception> {
    match *op {
        Op::Lui { rd, imm } => cpu.set_reg(rd, imm as u64),
        Op::Auipc { rd, imm } => cpu.set_reg(rd, cpu.pc.wrapping_add(imm as u64)),
        Op::Jal { rd, imm } => {
            let link = cpu.next_pc; // pc + len, before overwrite
            cpu.next_pc = cpu.pc.wrapping_add(imm as u64);
            cpu.set_reg(rd, link);
        }
        Op::Jalr { rd, rs1, imm } => {
            let link = cpu.next_pc; // pc + len, before overwrite
            let target = cpu.reg(rs1).wrapping_add(imm as u64) & !1;
            cpu.next_pc = target;
            cpu.set_reg(rd, link);
        }
        Op::Branch {
            cond,
            rs1,
            rs2,
            imm,
        } => {
            let a = cpu.reg(rs1);
            let b = cpu.reg(rs2);
            let taken = match cond {
                BranchCond::Eq => a == b,
                BranchCond::Ne => a != b,
                BranchCond::Lt => (a as i64) < (b as i64),
                BranchCond::Ge => (a as i64) >= (b as i64),
                BranchCond::Ltu => a < b,
                BranchCond::Geu => a >= b,
            };
            if taken {
                cpu.next_pc = cpu.pc.wrapping_add(imm as u64);
            }
        }
        Op::Load { kind, rd, rs1, imm } => {
            let addr = cpu.reg(rs1).wrapping_add(imm as u64);
            let size = match kind {
                LoadKind::B | LoadKind::Bu => 1,
                LoadKind::H | LoadKind::Hu => 2,
                LoadKind::W | LoadKind::Wu => 4,
                LoadKind::D => 8,
            };
            let v = cpu.load(bus, addr, size)?;
            let v = match kind {
                LoadKind::B => v as i8 as i64 as u64,
                LoadKind::H => v as i16 as i64 as u64,
                LoadKind::W => v as i32 as i64 as u64,
                LoadKind::D | LoadKind::Bu | LoadKind::Hu | LoadKind::Wu => v,
            };
            cpu.set_reg(rd, v);
        }
        Op::Store {
            kind,
            rs1,
            rs2,
            imm,
        } => {
            let addr = cpu.reg(rs1).wrapping_add(imm as u64);
            let size = match kind {
                StoreKind::B => 1,
                StoreKind::H => 2,
                StoreKind::W => 4,
                StoreKind::D => 8,
            };
            cpu.store(bus, addr, cpu.reg(rs2), size)?;
        }
        Op::OpImm { kind, rd, rs1, imm } => {
            let v = alu64(kind, cpu.reg(rs1), imm as u64);
            cpu.set_reg(rd, v);
        }
        Op::OpImm32 { kind, rd, rs1, imm } => {
            let v = alu32(kind, cpu.reg(rs1), imm as u64);
            cpu.set_reg(rd, v);
        }
        Op::OpReg { kind, rd, rs1, rs2 } => {
            let v = alu64(kind, cpu.reg(rs1), cpu.reg(rs2));
            cpu.set_reg(rd, v);
        }
        Op::OpReg32 { kind, rd, rs1, rs2 } => {
            let v = alu32(kind, cpu.reg(rs1), cpu.reg(rs2));
            cpu.set_reg(rd, v);
        }
        Op::Bit { kind, rd, rs1, rs2 } => {
            let v = bitmanip::bit_rr(kind, cpu.reg(rs1), cpu.reg(rs2));
            cpu.set_reg(rd, v);
        }
        Op::BitUnary { kind, rd, rs1 } => {
            let v = bitmanip::bit_unary(kind, cpu.reg(rs1));
            cpu.set_reg(rd, v);
        }
        Op::BitImm { kind, rd, rs1, imm } => {
            let v = bitmanip::bit_imm(kind, cpu.reg(rs1), imm);
            cpu.set_reg(rd, v);
        }
        Op::Fence => {}
        Op::FenceI => cpu.flush_decode_cache(),
        Op::Ecall => {
            return Err(match cpu.mode {
                Mode::User => Exception::EnvironmentCallFromU,
                Mode::Supervisor => Exception::EnvironmentCallFromS,
                Mode::Machine => Exception::EnvironmentCallFromM,
            });
        }
        Op::Ebreak => return Err(Exception::Breakpoint(cpu.pc)),
        Op::Csr { kind, rd, rs1, csr } => {
            let illegal = Exception::IllegalInstruction(raw as u64);
            let mtime = bus.clint.mtime;
            // For *I forms `rs1` holds the 5-bit zimm.
            let src = match kind {
                CsrOp::Rw | CsrOp::Rs | CsrOp::Rc => cpu.reg(rs1),
                CsrOp::Rwi | CsrOp::Rsi | CsrOp::Rci => rs1 as u64,
            };
            match kind {
                CsrOp::Rw | CsrOp::Rwi => {
                    // rd == x0: no read at all, so no read side effects and no
                    // read permission check; the write still happens.
                    let old = if rd != 0 {
                        Some(cpu.csr.read(csr, cpu.mode, mtime).map_err(|_| illegal)?)
                    } else {
                        None
                    };
                    let old_satp = (csr == csr::SATP).then(|| cpu.csr.load_raw(csr::SATP));
                    cpu.csr.write(csr, src, cpu.mode).map_err(|_| illegal)?;
                    if old_satp.is_some_and(|old| old != cpu.csr.load_raw(csr::SATP)) {
                        // Only the TLB: decoded blocks are keyed on physical
                        // address, so a new address space cannot make them
                        // stale.
                        cpu.mmu.flush_all();
                    }
                    if let Some(old) = old {
                        cpu.set_reg(rd, old);
                    }
                }
                CsrOp::Rs | CsrOp::Rsi | CsrOp::Rc | CsrOp::Rci => {
                    let old = cpu.csr.read(csr, cpu.mode, mtime).map_err(|_| illegal)?;
                    // rs1 == x0 / zimm == 0: read-only access, no write (so
                    // read-only CSRs are readable via csrrs/csrrc).
                    if rs1 != 0 {
                        let new = match kind {
                            CsrOp::Rs | CsrOp::Rsi => old | src,
                            _ => old & !src,
                        };
                        // Compare the raw satp before/after: `old` came from a
                        // masked, permission-checked read, so comparing
                        // against it could miss or invent a mapping change.
                        let old_satp = (csr == csr::SATP).then(|| cpu.csr.load_raw(csr::SATP));
                        cpu.csr.write(csr, new, cpu.mode).map_err(|_| illegal)?;
                        if old_satp.is_some_and(|old| old != cpu.csr.load_raw(csr::SATP)) {
                            // TLB only; see the csrrw path above.
                            cpu.mmu.flush_all();
                        }
                    }
                    cpu.set_reg(rd, old);
                }
            }
        }
        Op::Mret => {
            cpu.mret()
                .map_err(|_| Exception::IllegalInstruction(raw as u64))?;
        }
        Op::Sret => {
            cpu.sret()
                .map_err(|_| Exception::IllegalInstruction(raw as u64))?;
        }
        Op::Wfi => {
            if cpu.mode == Mode::User
                || (cpu.mode == Mode::Supervisor && cpu.csr.mstatus_field(mstatus::TW))
            {
                return Err(Exception::IllegalInstruction(raw as u64));
            }
            cpu.wfi = true;
        }
        Op::SfenceVma { .. } => {
            if cpu.mode == Mode::User
                || (cpu.mode == Mode::Supervisor && cpu.csr.mstatus_field(mstatus::TVM))
            {
                return Err(Exception::IllegalInstruction(raw as u64));
            }
            // Conservative: ignore rs1/rs2 specificity, flush everything.
            // TLB only: SFENCE.VMA orders page-table writes, not stores to
            // instruction memory. FENCE.I is the ordering point for those, and
            // plain stores to a code page are caught by its write generation.
            cpu.mmu.flush_all();
        }
        Op::Lr { width, rd, rs1 } => {
            let addr = cpu.reg(rs1);
            let size = amo_size(width);
            let pa = cpu.translate_amo(bus, addr, size, false)?;
            let v = bus
                .read(pa, size)
                .map_err(|_| Exception::LoadAccessFault(addr))?;
            let v = match width {
                AmoWidth::W => v as i32 as i64 as u64,
                AmoWidth::D => v,
            };
            cpu.set_reg(rd, v);
            cpu.reservation = Some(pa);
        }
        Op::Sc {
            width,
            rd,
            rs1,
            rs2,
        } => {
            let addr = cpu.reg(rs1);
            let size = amo_size(width);
            // Capture before translate/store paths can clear it.
            let saved = cpu.reservation;
            let pa = cpu.translate_amo(bus, addr, size, true)?;
            if !bus.write_accessible(pa, size) {
                return Err(Exception::StoreAccessFault(addr));
            }
            // SC always clears the reservation, success or failure.
            cpu.reservation = None;
            if saved == Some(pa) {
                bus.write(pa, cpu.reg(rs2), size)
                    .map_err(|_| Exception::StoreAccessFault(addr))?;
                cpu.set_reg(rd, 0);
            } else {
                cpu.set_reg(rd, 1);
            }
        }
        Op::Amo {
            op,
            width,
            rd,
            rs1,
            rs2,
        } => {
            let addr = cpu.reg(rs1);
            let size = amo_size(width);
            // AMOs are stores and must clear any LR reservation;
            // translate_amo does not do it.
            cpu.reservation = None;
            let pa = cpu.translate_amo(bus, addr, size, true)?;
            let old = bus
                .read(pa, size)
                .map_err(|_| Exception::StoreAccessFault(addr))?;
            let new = amo_result(op, width, old, cpu.reg(rs2));
            bus.write(pa, new, size)
                .map_err(|_| Exception::StoreAccessFault(addr))?;
            let old = match width {
                AmoWidth::W => old as i32 as i64 as u64,
                AmoWidth::D => old,
            };
            cpu.set_reg(rd, old);
        }
        Op::FpLoad { .. } | Op::FpStore { .. } | Op::Fp { .. } => {
            return fpu::execute(cpu, bus, raw, op);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: u64 = 0x8000_0000;
    const RAW: u32 = 0xDEAD_BEEF; // distinctive tval for IllegalInstruction

    fn setup() -> (Cpu, Bus) {
        let mut cpu = Cpu::new();
        let bus = Bus::new(1 << 20);
        cpu.pc = PC;
        cpu.next_pc = PC + 4; // mimic step(): next_pc = pc + len
        (cpu, bus)
    }

    fn exec(cpu: &mut Cpu, bus: &mut Bus, op: Op) -> Result<(), Exception> {
        execute(cpu, bus, RAW, &op)
    }

    #[test]
    fn alu64_basics_and_wrap() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, u64::MAX);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Add,
                rd: 2,
                rs1: 1,
                imm: 1,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(2), 0, "addi wraps");

        cpu.set_reg(3, 5);
        cpu.set_reg(4, 7);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Sub,
                rd: 5,
                rs1: 3,
                rs2: 4,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5) as i64, -2);

        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Xor,
                rd: 6,
                rs1: 3,
                imm: -1,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(6), !5u64, "xori -1 = not");
    }

    #[test]
    fn alu64_slt_signed_vs_unsigned() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, u64::MAX); // -1 signed
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Slt,
                rd: 2,
                rs1: 1,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(2), 1, "-1 < 0 signed");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Sltu,
                rd: 3,
                rs1: 1,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), 0, "u64::MAX not < 0 unsigned");
    }

    #[test]
    fn shifts_64() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, 1);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Sll,
                rd: 2,
                rs1: 1,
                imm: 63,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(2), 1u64 << 63);

        cpu.set_reg(3, 0xF000_0000_0000_0000);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Srl,
                rd: 4,
                rs1: 3,
                imm: 60,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 0xF, "srl is logical");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm {
                kind: AluOp::Sra,
                rd: 5,
                rs1: 3,
                imm: 60,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), u64::MAX, "sra is arithmetic");

        // Register-form shamt masked to 6 bits: 65 -> 1.
        cpu.set_reg(6, 65);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Sll,
                rd: 7,
                rs1: 1,
                rs2: 6,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(7), 2);
    }

    #[test]
    fn alu32_sign_extension_and_shifts() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, 0x7FFF_FFFF);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm32 {
                kind: AluOp::Add,
                rd: 2,
                rs1: 1,
                imm: 1,
            },
        )
        .unwrap();
        assert_eq!(
            cpu.reg(2),
            0xFFFF_FFFF_8000_0000,
            "addiw overflows into sign"
        );

        // srliw operates on the low 32 bits only.
        cpu.set_reg(3, 0xFFFF_FFFF_8000_0000);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm32 {
                kind: AluOp::Srl,
                rd: 4,
                rs1: 3,
                imm: 4,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 0x0800_0000);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpImm32 {
                kind: AluOp::Sra,
                rd: 5,
                rs1: 3,
                imm: 4,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), 0xFFFF_FFFF_F800_0000, "sraw sign-extends");

        // Register-form W shamt masked to 5 bits: 33 -> 1.
        cpu.set_reg(6, 33);
        cpu.set_reg(7, 1);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg32 {
                kind: AluOp::Sll,
                rd: 8,
                rs1: 7,
                rs2: 6,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(8), 2);
    }

    #[test]
    fn m_extension_multiply() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, u64::MAX); // -1
        cpu.set_reg(2, u64::MAX); // -1
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Mul,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), 1, "-1 * -1 low");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Mulh,
                rd: 4,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 0, "-1 * -1 high (signed)");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Mulhu,
                rd: 5,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), u64::MAX - 1, "umax * umax high");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Mulhsu,
                rd: 6,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(6), u64::MAX, "(-1) * umax high = -1");
    }

    #[test]
    fn m_extension_division_edges() {
        let (mut cpu, mut bus) = setup();
        // Division by zero.
        cpu.set_reg(1, 42);
        cpu.set_reg(2, 0);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Div,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), u64::MAX, "div by zero -> -1");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Divu,
                rd: 4,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), u64::MAX, "divu by zero -> all ones");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Rem,
                rd: 5,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), 42, "rem by zero -> dividend");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Remu,
                rd: 6,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(6), 42, "remu by zero -> dividend");

        // Signed overflow.
        cpu.set_reg(1, i64::MIN as u64);
        cpu.set_reg(2, u64::MAX); // -1
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Div,
                rd: 7,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(7), i64::MIN as u64, "i64::MIN / -1 -> i64::MIN");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg {
                kind: AluOp::Rem,
                rd: 8,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(8), 0, "i64::MIN % -1 -> 0");
    }

    #[test]
    fn m_extension_division_w_forms() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, i32::MIN as u32 as u64);
        cpu.set_reg(2, u32::MAX as u64); // -1 in 32 bits
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg32 {
                kind: AluOp::Div,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(
            cpu.reg(3),
            i32::MIN as i64 as u64,
            "divw overflow, sign-extended"
        );
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg32 {
                kind: AluOp::Rem,
                rd: 4,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 0);

        cpu.set_reg(5, 0xFFFF_FFFF_0000_0007); // low 32 = 7
        cpu.set_reg(6, 0);
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg32 {
                kind: AluOp::Div,
                rd: 7,
                rs1: 5,
                rs2: 6,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(7), u64::MAX, "divw by zero -> -1 sign-extended");
        exec(
            &mut cpu,
            &mut bus,
            Op::OpReg32 {
                kind: AluOp::Remu,
                rd: 8,
                rs1: 5,
                rs2: 6,
            },
        )
        .unwrap();
        assert_eq!(
            cpu.reg(8),
            7,
            "remuw by zero -> low-32 dividend, sign-extended"
        );
    }

    #[test]
    fn lui_auipc() {
        let (mut cpu, mut bus) = setup();
        exec(&mut cpu, &mut bus, Op::Lui { rd: 1, imm: -4096 }).unwrap();
        assert_eq!(cpu.reg(1), (-4096i64) as u64);
        exec(&mut cpu, &mut bus, Op::Auipc { rd: 2, imm: 0x1000 }).unwrap();
        assert_eq!(cpu.reg(2), PC + 0x1000);
    }

    #[test]
    fn branches_next_pc() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, 5);
        cpu.set_reg(2, 5);
        exec(
            &mut cpu,
            &mut bus,
            Op::Branch {
                cond: BranchCond::Eq,
                rs1: 1,
                rs2: 2,
                imm: 0x100,
            },
        )
        .unwrap();
        assert_eq!(cpu.next_pc, PC + 0x100, "taken branch retargets next_pc");

        cpu.next_pc = PC + 4;
        exec(
            &mut cpu,
            &mut bus,
            Op::Branch {
                cond: BranchCond::Ne,
                rs1: 1,
                rs2: 2,
                imm: 0x100,
            },
        )
        .unwrap();
        assert_eq!(cpu.next_pc, PC + 4, "not-taken branch leaves next_pc");

        // Signed vs unsigned comparison.
        cpu.set_reg(3, u64::MAX); // -1
        cpu.set_reg(4, 1);
        cpu.next_pc = PC + 4;
        exec(
            &mut cpu,
            &mut bus,
            Op::Branch {
                cond: BranchCond::Lt,
                rs1: 3,
                rs2: 4,
                imm: -8,
            },
        )
        .unwrap();
        assert_eq!(cpu.next_pc, PC - 8, "blt: -1 < 1 signed, negative offset");
        cpu.next_pc = PC + 4;
        exec(
            &mut cpu,
            &mut bus,
            Op::Branch {
                cond: BranchCond::Ltu,
                rs1: 3,
                rs2: 4,
                imm: -8,
            },
        )
        .unwrap();
        assert_eq!(cpu.next_pc, PC + 4, "bltu: u64::MAX not < 1");
    }

    #[test]
    fn jal_jalr() {
        let (mut cpu, mut bus) = setup();
        exec(&mut cpu, &mut bus, Op::Jal { rd: 1, imm: 0x80 }).unwrap();
        assert_eq!(cpu.reg(1), PC + 4, "jal link = pc + len");
        assert_eq!(cpu.next_pc, PC + 0x80);

        // jalr with rd == rs1: target uses the OLD rs1, link is old next_pc.
        cpu.next_pc = PC + 4;
        cpu.set_reg(1, 0x8000_1001);
        exec(
            &mut cpu,
            &mut bus,
            Op::Jalr {
                rd: 1,
                rs1: 1,
                imm: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.next_pc, 0x8000_1002, "target = (rs1 + imm) & !1");
        assert_eq!(cpu.reg(1), PC + 4, "link written after target computed");
    }

    #[test]
    fn fences_are_nops() {
        let (mut cpu, mut bus) = setup();
        exec(&mut cpu, &mut bus, Op::Fence).unwrap();
        exec(&mut cpu, &mut bus, Op::FenceI).unwrap();
        assert_eq!(cpu.next_pc, PC + 4);
    }

    #[test]
    fn ecall_ebreak() {
        let (mut cpu, mut bus) = setup();
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Ecall),
            Err(Exception::EnvironmentCallFromM)
        );
        cpu.mode = Mode::Supervisor;
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Ecall),
            Err(Exception::EnvironmentCallFromS)
        );
        cpu.mode = Mode::User;
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Ecall),
            Err(Exception::EnvironmentCallFromU)
        );
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Ebreak),
            Err(Exception::Breakpoint(PC))
        );
    }

    #[test]
    fn csr_rw_basic() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, 0x1234);
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rw,
                rd: 2,
                rs1: 1,
                csr: csr::MSCRATCH,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(2), 0, "rd gets old value");
        assert_eq!(cpu.csr.load_raw(csr::MSCRATCH), 0x1234);

        // csrrwi with rd == x0 still performs the write.
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rwi,
                rd: 0,
                rs1: 0x1F,
                csr: csr::MSCRATCH,
            },
        )
        .unwrap();
        assert_eq!(cpu.csr.load_raw(csr::MSCRATCH), 0x1F);
    }

    #[test]
    fn csr_set_clear() {
        let (mut cpu, mut bus) = setup();
        cpu.csr.store_raw(csr::MSCRATCH, 0b1100);
        cpu.set_reg(1, 0b0110);
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rs,
                rd: 2,
                rs1: 1,
                csr: csr::MSCRATCH,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(2), 0b1100, "csrrs rd gets old");
        assert_eq!(cpu.csr.load_raw(csr::MSCRATCH), 0b1110);

        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rci,
                rd: 3,
                rs1: 0b0010,
                csr: csr::MSCRATCH,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), 0b1110);
        assert_eq!(
            cpu.csr.load_raw(csr::MSCRATCH),
            0b1100,
            "csrrci clears bits"
        );
    }

    #[test]
    fn csr_read_only_and_privilege() {
        let (mut cpu, mut bus) = setup();
        // csrrs with rs1 == x0 reads a read-only CSR without writing.
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rs,
                rd: 1,
                rs1: 0,
                csr: csr::MHARTID,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(1), 0);

        // csrrw to a read-only CSR is illegal, tval = raw.
        cpu.set_reg(2, 1);
        assert_eq!(
            exec(
                &mut cpu,
                &mut bus,
                Op::Csr {
                    kind: CsrOp::Rw,
                    rd: 0,
                    rs1: 2,
                    csr: csr::MHARTID
                }
            ),
            Err(Exception::IllegalInstruction(RAW as u64))
        );

        // U-mode access to a machine CSR is illegal.
        cpu.mode = Mode::User;
        assert_eq!(
            exec(
                &mut cpu,
                &mut bus,
                Op::Csr {
                    kind: CsrOp::Rs,
                    rd: 1,
                    rs1: 0,
                    csr: csr::MSTATUS
                }
            ),
            Err(Exception::IllegalInstruction(RAW as u64))
        );
    }

    #[test]
    fn csr_mstatus_write_masked() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, mstatus::MIE | mstatus::SIE);
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rs,
                rd: 0,
                rs1: 1,
                csr: csr::MSTATUS,
            },
        )
        .unwrap();
        assert!(cpu.csr.mstatus_field(mstatus::MIE));
        assert!(cpu.csr.mstatus_field(mstatus::SIE));
    }

    #[test]
    fn csr_satp_write_flushes_tlb() {
        let (mut cpu, mut bus) = setup();
        cpu.mmu.tlb[0].vpn = 5; // fake a live entry
        cpu.set_reg(1, (8 << 60) | 1); // change Bare to Sv39
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rw,
                rd: 0,
                rs1: 1,
                csr: csr::SATP,
            },
        )
        .unwrap();
        assert_eq!(cpu.mmu.tlb[0].vpn, u64::MAX, "satp write flushes the TLB");
    }

    #[test]
    fn unchanged_or_ignored_satp_write_preserves_tlb() {
        let (mut cpu, mut bus) = setup();
        cpu.mmu.tlb[0].vpn = 5;

        for value in [0, 9 << 60] {
            cpu.set_reg(1, value);
            exec(
                &mut cpu,
                &mut bus,
                Op::Csr {
                    kind: CsrOp::Rw,
                    rd: 0,
                    rs1: 1,
                    csr: csr::SATP,
                },
            )
            .unwrap();
            assert_eq!(cpu.mmu.tlb[0].vpn, 5);
        }
    }

    #[test]
    fn csr_time_reads_mtime() {
        let (mut cpu, mut bus) = setup();
        bus.clint.mtime = 0xABCD;
        exec(
            &mut cpu,
            &mut bus,
            Op::Csr {
                kind: CsrOp::Rs,
                rd: 1,
                rs1: 0,
                csr: csr::TIME,
            },
        )
        .unwrap();
        assert_eq!(
            cpu.reg(1),
            0xABCD,
            "TIME backed by clint mtime (M-mode ungated)"
        );
    }

    #[test]
    fn mret_sret() {
        let (mut cpu, mut bus) = setup();
        // mret from M: returns to MEPC, drops to MPP mode.
        cpu.csr.store_raw(csr::MEPC, 0x8000_4000);
        // MPP = Supervisor (1), MPIE = 1.
        let s = cpu.csr.load_raw(csr::MSTATUS) & !mstatus::MPP_MASK;
        cpu.csr
            .store_raw(csr::MSTATUS, s | (1 << mstatus::MPP_SHIFT) | mstatus::MPIE);
        exec(&mut cpu, &mut bus, Op::Mret).unwrap();
        assert_eq!(cpu.next_pc, 0x8000_4000);
        assert_eq!(cpu.mode, Mode::Supervisor);
        assert!(
            cpu.csr.mstatus_field(mstatus::MIE),
            "MIE restored from MPIE"
        );

        // mret from below M is illegal with tval = raw.
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Mret),
            Err(Exception::IllegalInstruction(RAW as u64))
        );

        // sret from U is illegal.
        cpu.mode = Mode::User;
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Sret),
            Err(Exception::IllegalInstruction(RAW as u64))
        );

        // sret from S: returns to SEPC, mode from SPP.
        cpu.mode = Mode::Supervisor;
        cpu.csr.store_raw(csr::SEPC, 0x8000_2000);
        let s = cpu.csr.load_raw(csr::MSTATUS) & !mstatus::SPP; // SPP = U
        cpu.csr.store_raw(csr::MSTATUS, s);
        exec(&mut cpu, &mut bus, Op::Sret).unwrap();
        assert_eq!(cpu.next_pc, 0x8000_2000);
        assert_eq!(cpu.mode, Mode::User);
    }

    #[test]
    fn wfi() {
        let (mut cpu, mut bus) = setup();
        exec(&mut cpu, &mut bus, Op::Wfi).unwrap();
        assert!(cpu.wfi, "wfi in M-mode halts");

        // TW set: WFI in S-mode is illegal.
        let (mut cpu, mut bus) = setup();
        let s = cpu.csr.load_raw(csr::MSTATUS);
        cpu.csr.store_raw(csr::MSTATUS, s | mstatus::TW);
        cpu.mode = Mode::Supervisor;
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Wfi),
            Err(Exception::IllegalInstruction(RAW as u64))
        );

        // WFI in U-mode is always illegal, even with TW clear.
        let (mut cpu, mut bus) = setup();
        cpu.mode = Mode::User;
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::Wfi),
            Err(Exception::IllegalInstruction(RAW as u64))
        );
    }

    #[test]
    fn sfence_vma() {
        let (mut cpu, mut bus) = setup();
        cpu.mmu.tlb[0].vpn = 9;
        exec(&mut cpu, &mut bus, Op::SfenceVma { rs1: 0, rs2: 0 }).unwrap();
        assert_eq!(cpu.mmu.tlb[0].vpn, u64::MAX, "sfence flushes TLB");

        cpu.mode = Mode::User;
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::SfenceVma { rs1: 0, rs2: 0 }),
            Err(Exception::IllegalInstruction(RAW as u64))
        );

        // TVM set: S-mode sfence is illegal.
        cpu.mode = Mode::Supervisor;
        let s = cpu.csr.load_raw(csr::MSTATUS);
        cpu.csr.store_raw(csr::MSTATUS, s | mstatus::TVM);
        assert_eq!(
            exec(&mut cpu, &mut bus, Op::SfenceVma { rs1: 0, rs2: 0 }),
            Err(Exception::IllegalInstruction(RAW as u64))
        );
    }

    // ------------------------------------------------------------------
    // Memory-path tests: exercise cpu.load/store/translate_amo via the MMU
    // (bare mode, M privilege -> identity mapping into RAM).
    // ------------------------------------------------------------------

    #[test]
    fn load_store_sign_extension() {
        let (mut cpu, mut bus) = setup();
        let addr = 0x8000_1000u64;
        cpu.set_reg(1, addr);
        cpu.set_reg(2, 0xFFu64);
        exec(
            &mut cpu,
            &mut bus,
            Op::Store {
                kind: StoreKind::B,
                rs1: 1,
                rs2: 2,
                imm: 0,
            },
        )
        .unwrap();
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::B,
                rd: 3,
                rs1: 1,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), u64::MAX, "lb sign-extends");
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::Bu,
                rd: 4,
                rs1: 1,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 0xFF, "lbu zero-extends");

        cpu.set_reg(2, 0x8000_0000u64);
        exec(
            &mut cpu,
            &mut bus,
            Op::Store {
                kind: StoreKind::W,
                rs1: 1,
                rs2: 2,
                imm: 8,
            },
        )
        .unwrap();
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::W,
                rd: 5,
                rs1: 1,
                imm: 8,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), 0xFFFF_FFFF_8000_0000, "lw sign-extends");
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::Wu,
                rd: 6,
                rs1: 1,
                imm: 8,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(6), 0x8000_0000, "lwu zero-extends");
    }

    #[test]
    fn lr_sc_reservation() {
        let (mut cpu, mut bus) = setup();
        let addr = 0x8000_2000u64;
        cpu.set_reg(1, addr);
        cpu.set_reg(2, 77);

        // SC without a reservation fails (rd = 1).
        exec(
            &mut cpu,
            &mut bus,
            Op::Sc {
                width: AmoWidth::D,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), 1);

        // LR then SC succeeds (rd = 0) and stores.
        exec(
            &mut cpu,
            &mut bus,
            Op::Lr {
                width: AmoWidth::D,
                rd: 4,
                rs1: 1,
            },
        )
        .unwrap();
        assert!(cpu.reservation.is_some());
        exec(
            &mut cpu,
            &mut bus,
            Op::Sc {
                width: AmoWidth::D,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), 0);
        assert!(cpu.reservation.is_none(), "sc clears reservation");
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::D,
                rd: 5,
                rs1: 1,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), 77);
    }

    #[test]
    fn misaligned_lr_reports_load_access_fault() {
        let (mut cpu, mut bus) = setup();
        let addr = 0x8000_2001;
        cpu.set_reg(1, addr);

        let result = exec(
            &mut cpu,
            &mut bus,
            Op::Lr {
                width: AmoWidth::D,
                rd: 4,
                rs1: 1,
            },
        );

        assert_eq!(result, Err(Exception::LoadAccessFault(addr)));
    }

    #[test]
    fn misaligned_sc_reports_store_access_fault() {
        let (mut cpu, mut bus) = setup();
        let addr = 0x8000_2001;
        cpu.set_reg(1, addr);

        let result = exec(
            &mut cpu,
            &mut bus,
            Op::Sc {
                width: AmoWidth::D,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        );

        assert_eq!(result, Err(Exception::StoreAccessFault(addr)));
    }

    #[test]
    fn failed_sc_still_checks_physical_write_access() {
        let (mut cpu, mut bus) = setup();
        let addr = crate::bus::DRAM_BASE + bus.ram.size();
        cpu.set_reg(1, addr);

        let result = exec(
            &mut cpu,
            &mut bus,
            Op::Sc {
                width: AmoWidth::D,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        );

        assert_eq!(result, Err(Exception::StoreAccessFault(addr)));
    }

    #[test]
    fn amo_ops() {
        let (mut cpu, mut bus) = setup();
        let addr = 0x8000_3000u64;
        cpu.set_reg(1, addr);
        cpu.set_reg(2, 10);
        exec(
            &mut cpu,
            &mut bus,
            Op::Store {
                kind: StoreKind::D,
                rs1: 1,
                rs2: 2,
                imm: 0,
            },
        )
        .unwrap();

        cpu.set_reg(3, 5);
        exec(
            &mut cpu,
            &mut bus,
            Op::Amo {
                op: AmoOp::Add,
                width: AmoWidth::D,
                rd: 4,
                rs1: 1,
                rs2: 3,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 10, "amoadd rd = old");
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::D,
                rd: 5,
                rs1: 1,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), 15);

        // amomin.w: signed 32-bit compare, rd sign-extended old.
        cpu.set_reg(2, 0xFFFF_FFFFu64); // -1 as i32
        exec(
            &mut cpu,
            &mut bus,
            Op::Store {
                kind: StoreKind::W,
                rs1: 1,
                rs2: 2,
                imm: 8,
            },
        )
        .unwrap();
        cpu.set_reg(3, 1);
        cpu.set_reg(6, addr + 8);
        exec(
            &mut cpu,
            &mut bus,
            Op::Amo {
                op: AmoOp::Min,
                width: AmoWidth::W,
                rd: 7,
                rs1: 6,
                rs2: 3,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(7), u64::MAX, "old (-1) sign-extended into rd");
        exec(
            &mut cpu,
            &mut bus,
            Op::Load {
                kind: LoadKind::W,
                rd: 8,
                rs1: 6,
                imm: 0,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(8), u64::MAX, "min(-1, 1) = -1 stays");

        // This profile reports misaligned atomics as access faults.
        cpu.set_reg(9, addr + 1);
        assert_eq!(
            exec(
                &mut cpu,
                &mut bus,
                Op::Amo {
                    op: AmoOp::Swap,
                    width: AmoWidth::D,
                    rd: 0,
                    rs1: 9,
                    rs2: 3
                }
            ),
            Err(Exception::StoreAccessFault(addr + 1))
        );
    }

    /// The Zb*/Zk* ops go through `set_reg`, so x0 stays hard-wired to zero
    /// and the *W forms sign-extend into the upper half.
    #[test]
    fn bitmanip_writeback() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(1, 0x0123_4567_89ab_cdef);
        cpu.set_reg(2, 8);

        exec(
            &mut cpu,
            &mut bus,
            Op::Bit {
                kind: BitOp::Ror,
                rd: 3,
                rs1: 1,
                rs2: 2,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(3), 0xef01_2345_6789_abcd);

        exec(
            &mut cpu,
            &mut bus,
            Op::BitUnary {
                kind: BitUnaryOp::Rev8,
                rd: 4,
                rs1: 1,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(4), 0xefcd_ab89_6745_2301);

        exec(
            &mut cpu,
            &mut bus,
            Op::BitImm {
                kind: BitImmOp::Rori,
                rd: 5,
                rs1: 1,
                imm: 8,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(5), cpu.reg(3), "rori 8 == ror by 8");

        // rorw sign-extends from bit 31.
        cpu.set_reg(6, 3);
        cpu.set_reg(7, 1);
        exec(
            &mut cpu,
            &mut bus,
            Op::Bit {
                kind: BitOp::Rorw,
                rd: 8,
                rs1: 6,
                rs2: 7,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(8), 0xffff_ffff_8000_0001);

        // Writes to x0 are discarded.
        exec(
            &mut cpu,
            &mut bus,
            Op::BitUnary {
                kind: BitUnaryOp::Cpop,
                rd: 0,
                rs1: 1,
            },
        )
        .unwrap();
        assert_eq!(cpu.reg(0), 0);
    }
}
