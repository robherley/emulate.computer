//! Instruction decoder: raw bits -> `DecodedInst`.
//!
//! `decode` accepts a 32-bit fetch window. If the low two bits are `11` it is
//! a standard 32-bit instruction (`len == 4`); otherwise the low 16 bits are a
//! compressed (C-extension) instruction which is expanded to the same `Op`
//! representation (`len == 2`). Illegal encodings return `Err(())`, mapped by
//! the caller to `Exception::IllegalInstruction(raw)`.

pub use super::bitmanip::{BitImmOp, BitOp, BitUnaryOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchCond {
    Eq,
    Ne,
    Lt,
    Ge,
    Ltu,
    Geu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadKind {
    B,
    H,
    W,
    D,
    Bu,
    Hu,
    Wu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    B,
    H,
    W,
    D,
}

/// Integer ALU ops, shared by OpImm/OpReg (and the 32-bit `*W` forms).
/// M-extension ops only appear in `OpReg`/`OpReg32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Mul,
    Mulh,
    Mulhsu,
    Mulhu,
    Div,
    Divu,
    Rem,
    Remu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsrOp {
    Rw,
    Rs,
    Rc,
    Rwi,
    Rsi,
    Rci,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmoWidth {
    W,
    D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmoOp {
    Swap,
    Add,
    Xor,
    And,
    Or,
    Min,
    Max,
    Minu,
    Maxu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpSize {
    S,
    D,
}

/// Floating-point operations (F and D share this via `FpSize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpOp {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
    Sgnj,
    Sgnjn,
    Sgnjx,
    Min,
    Max,
    Madd,
    Msub,
    Nmsub,
    Nmadd,
    /// `fcvt.w.<fmt>`  (fp -> i32, signed)
    CvtWFmt,
    /// `fcvt.wu.<fmt>` (fp -> u32)
    CvtWuFmt,
    /// `fcvt.l.<fmt>`  (fp -> i64)
    CvtLFmt,
    /// `fcvt.lu.<fmt>` (fp -> u64)
    CvtLuFmt,
    /// `fcvt.<fmt>.w`  (i32 -> fp)
    CvtFmtW,
    /// `fcvt.<fmt>.wu` (u32 -> fp)
    CvtFmtWu,
    /// `fcvt.<fmt>.l`  (i64 -> fp)
    CvtFmtL,
    /// `fcvt.<fmt>.lu` (u64 -> fp)
    CvtFmtLu,
    /// fcvt.s.d (sz == S) / fcvt.d.s (sz == D): convert *to* `sz` from the other format
    CvtFmtFmt,
    /// fmv.x.w / fmv.x.d (raw bits fp -> int)
    MvXFmt,
    /// fmv.w.x / fmv.d.x (raw bits int -> fp)
    MvFmtX,
    Class,
    Eq,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Lui {
        rd: usize,
        imm: i64,
    },
    Auipc {
        rd: usize,
        imm: i64,
    },
    Jal {
        rd: usize,
        imm: i64,
    },
    Jalr {
        rd: usize,
        rs1: usize,
        imm: i64,
    },
    Branch {
        cond: BranchCond,
        rs1: usize,
        rs2: usize,
        imm: i64,
    },
    Load {
        kind: LoadKind,
        rd: usize,
        rs1: usize,
        imm: i64,
    },
    Store {
        kind: StoreKind,
        rs1: usize,
        rs2: usize,
        imm: i64,
    },
    /// imm holds the sign-extended immediate; for shifts it is the 6-bit shamt.
    OpImm {
        kind: AluOp,
        rd: usize,
        rs1: usize,
        imm: i64,
    },
    /// 32-bit (`*W`) variants; shamt is 5 bits.
    OpImm32 {
        kind: AluOp,
        rd: usize,
        rs1: usize,
        imm: i64,
    },
    OpReg {
        kind: AluOp,
        rd: usize,
        rs1: usize,
        rs2: usize,
    },
    OpReg32 {
        kind: AluOp,
        rd: usize,
        rs1: usize,
        rs2: usize,
    },
    /// Zbb/Zbc/Zbkb/Zkn register-register forms (OP and OP-32); the `*W`
    /// variants are distinct `BitOp`s, so this covers both major opcodes.
    Bit {
        kind: BitOp,
        rd: usize,
        rs1: usize,
        rs2: usize,
    },
    /// Zbb/Zbkb/Zkn unary forms: OP-IMM / OP-IMM-32 with a fixed `funct12`.
    BitUnary {
        kind: BitUnaryOp,
        rd: usize,
        rs1: usize,
    },
    /// `rori`/`roriw` shamt, or the `aes64ks1i` round number.
    BitImm {
        kind: BitImmOp,
        rd: usize,
        rs1: usize,
        imm: u32,
    },
    Fence,
    FenceI,
    Ecall,
    Ebreak,
    /// For the *I forms, `rs1` holds the 5-bit zimm.
    Csr {
        kind: CsrOp,
        rd: usize,
        rs1: usize,
        csr: u16,
    },
    Mret,
    Sret,
    Wfi,
    SfenceVma {
        rs1: usize,
        rs2: usize,
    },
    Lr {
        width: AmoWidth,
        rd: usize,
        rs1: usize,
    },
    Sc {
        width: AmoWidth,
        rd: usize,
        rs1: usize,
        rs2: usize,
    },
    Amo {
        op: AmoOp,
        width: AmoWidth,
        rd: usize,
        rs1: usize,
        rs2: usize,
    },
    /// flw/fld — rd is an FP register.
    FpLoad {
        sz: FpSize,
        frd: usize,
        rs1: usize,
        imm: i64,
    },
    /// fsw/fsd — rs2 is an FP register.
    FpStore {
        sz: FpSize,
        rs1: usize,
        frs2: usize,
        imm: i64,
    },
    /// All other F/D ops. Register roles depend on `op` (integer vs FP);
    /// `rm` is the raw rounding-mode field (7 = dynamic). `rs3` only for fused ops.
    Fp {
        op: FpOp,
        sz: FpSize,
        rd: usize,
        rs1: usize,
        rs2: usize,
        rs3: usize,
        rm: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedInst {
    pub op: Op,
    /// 2 (compressed) or 4.
    pub len: u64,
}

/// Decode one instruction from a 32-bit fetch window.
pub fn decode(raw: u32) -> Result<DecodedInst, ()> {
    if raw & 3 == 3 {
        decode32(raw).map(|op| DecodedInst { op, len: 4 })
    } else {
        decode16(raw as u16).map(|op| DecodedInst { op, len: 2 })
    }
}

// ---------------------------------------------------------------------------
// 32-bit field / immediate helpers
// ---------------------------------------------------------------------------

fn rd(raw: u32) -> usize {
    ((raw >> 7) & 0x1f) as usize
}

fn rs1(raw: u32) -> usize {
    ((raw >> 15) & 0x1f) as usize
}

fn rs2(raw: u32) -> usize {
    ((raw >> 20) & 0x1f) as usize
}

fn funct3(raw: u32) -> u32 {
    (raw >> 12) & 0x7
}

fn funct7(raw: u32) -> u32 {
    (raw >> 25) & 0x7f
}

/// I-type immediate: bits 31:20, sign-extended.
fn imm_i(raw: u32) -> i64 {
    ((raw as i32) >> 20) as i64
}

/// S-type immediate: bits 31:25 | 11:7, sign-extended.
fn imm_s(raw: u32) -> i64 {
    ((((raw & 0xfe00_0000) as i32) >> 20) as i64) | (((raw >> 7) & 0x1f) as i64)
}

/// B-type immediate: 12|10:5|4:1|11 scrambled fields, sign-extended.
fn imm_b(raw: u32) -> i64 {
    ((((raw & 0x8000_0000) as i32) >> 19) as i64) // imm[12], sign-extended
        | ((((raw >> 25) & 0x3f) << 5) as i64)    // imm[10:5]
        | ((((raw >> 8) & 0xf) << 1) as i64)      // imm[4:1]
        | ((((raw >> 7) & 0x1) << 11) as i64) // imm[11]
}

/// U-type immediate: bits 31:12 << 12, sign-extended.
fn imm_u(raw: u32) -> i64 {
    (raw & 0xffff_f000) as i32 as i64
}

/// J-type immediate: 20|10:1|11|19:12 scrambled fields, sign-extended.
fn imm_j(raw: u32) -> i64 {
    ((((raw & 0x8000_0000) as i32) >> 11) as i64) // imm[20], sign-extended
        | ((((raw >> 21) & 0x3ff) << 1) as i64)   // imm[10:1]
        | ((((raw >> 20) & 0x1) << 11) as i64)    // imm[11]
        | ((((raw >> 12) & 0xff) << 12) as i64) // imm[19:12]
}

/// Zbb/Zbc/Zbkb/Zkn register-register forms sharing the OP (`w == false`) and
/// OP-32 (`w == true`) major opcodes. Encodings per riscv-opcodes
/// `extensions/{rv_zbb,rv64_zbb,rv_zbc,rv_zbkb,rv64_zbkb,rv64_zknd,rv64_zkne}`.
fn decode_op_bit(raw: u32, w: bool) -> Result<Op, ()> {
    let kind = match (funct7(raw), funct3(raw), w) {
        // Zbb / Zbkb logic-with-negate
        (0x20, 7, false) => BitOp::Andn,
        (0x20, 6, false) => BitOp::Orn,
        (0x20, 4, false) => BitOp::Xnor,
        // Zbb min/max
        (0x05, 4, false) => BitOp::Min,
        (0x05, 5, false) => BitOp::Minu,
        (0x05, 6, false) => BitOp::Max,
        (0x05, 7, false) => BitOp::Maxu,
        // Zbc carry-less multiply
        (0x05, 1, false) => BitOp::Clmul,
        (0x05, 2, false) => BitOp::Clmulr,
        (0x05, 3, false) => BitOp::Clmulh,
        // Zbb / Zbkb rotates
        (0x30, 1, false) => BitOp::Rol,
        (0x30, 5, false) => BitOp::Ror,
        (0x30, 1, true) => BitOp::Rolw,
        (0x30, 5, true) => BitOp::Rorw,
        // Zbkb packing; `zext.h rd, rs1` is `packw rd, rs1, x0`.
        (0x04, 4, false) => BitOp::Pack,
        (0x04, 7, false) => BitOp::Packh,
        (0x04, 4, true) => BitOp::Packw,
        // Zkne / Zknd
        (0x19, 0, false) => BitOp::Aes64es,
        (0x1b, 0, false) => BitOp::Aes64esm,
        (0x1d, 0, false) => BitOp::Aes64ds,
        (0x1f, 0, false) => BitOp::Aes64dsm,
        (0x3f, 0, false) => BitOp::Aes64ks2,
        _ => return Err(()),
    };
    Ok(Op::Bit {
        kind,
        rd: rd(raw),
        rs1: rs1(raw),
        rs2: rs2(raw),
    })
}

/// Zbb/Zbkb/Zkn OP-IMM forms with a fixed 12-bit `funct12` in bits 31:20,
/// plus `aes64ks1i`, whose low four `funct12` bits carry `rnum`.
fn decode_op_imm_bit(raw: u32) -> Result<Op, ()> {
    let funct12 = raw >> 20;
    // aes64ks1i: funct3 = 1, bits 31:24 = 0b0011_0001, rnum = bits 23:20.
    if funct3(raw) == 1 && (funct12 >> 4) == 0x31 {
        let rnum = funct12 & 0xf;
        // rnum 0xB..0xF do not decode.
        if rnum > 0xa {
            return Err(());
        }
        return Ok(Op::BitImm {
            kind: BitImmOp::Aes64ks1i,
            rd: rd(raw),
            rs1: rs1(raw),
            imm: rnum,
        });
    }
    let kind = match (funct3(raw), funct12) {
        // Zbb
        (1, 0x600) => BitUnaryOp::Clz,
        (1, 0x601) => BitUnaryOp::Ctz,
        (1, 0x602) => BitUnaryOp::Cpop,
        (1, 0x604) => BitUnaryOp::SextB,
        (1, 0x605) => BitUnaryOp::SextH,
        (5, 0x287) => BitUnaryOp::OrcB,
        (5, 0x6b8) => BitUnaryOp::Rev8,
        // Zbkb
        (5, 0x687) => BitUnaryOp::Brev8,
        // Zknd
        (1, 0x300) => BitUnaryOp::Aes64im,
        // Zknh
        (1, 0x100) => BitUnaryOp::Sha256sum0,
        (1, 0x101) => BitUnaryOp::Sha256sum1,
        (1, 0x102) => BitUnaryOp::Sha256sig0,
        (1, 0x103) => BitUnaryOp::Sha256sig1,
        (1, 0x104) => BitUnaryOp::Sha512sum0,
        (1, 0x105) => BitUnaryOp::Sha512sum1,
        (1, 0x106) => BitUnaryOp::Sha512sig0,
        (1, 0x107) => BitUnaryOp::Sha512sig1,
        _ => return Err(()),
    };
    Ok(Op::BitUnary {
        kind,
        rd: rd(raw),
        rs1: rs1(raw),
    })
}

fn decode32(raw: u32) -> Result<Op, ()> {
    let opcode = raw & 0x7f;
    let f3 = funct3(raw);
    match opcode {
        // LUI
        0x37 => Ok(Op::Lui {
            rd: rd(raw),
            imm: imm_u(raw),
        }),
        // AUIPC
        0x17 => Ok(Op::Auipc {
            rd: rd(raw),
            imm: imm_u(raw),
        }),
        // JAL
        0x6f => Ok(Op::Jal {
            rd: rd(raw),
            imm: imm_j(raw),
        }),
        // JALR
        0x67 => match f3 {
            0 => Ok(Op::Jalr {
                rd: rd(raw),
                rs1: rs1(raw),
                imm: imm_i(raw),
            }),
            _ => Err(()),
        },
        // BRANCH
        0x63 => {
            let cond = match f3 {
                0 => BranchCond::Eq,
                1 => BranchCond::Ne,
                4 => BranchCond::Lt,
                5 => BranchCond::Ge,
                6 => BranchCond::Ltu,
                7 => BranchCond::Geu,
                _ => return Err(()),
            };
            Ok(Op::Branch {
                cond,
                rs1: rs1(raw),
                rs2: rs2(raw),
                imm: imm_b(raw),
            })
        }
        // LOAD
        0x03 => {
            let kind = match f3 {
                0 => LoadKind::B,
                1 => LoadKind::H,
                2 => LoadKind::W,
                3 => LoadKind::D,
                4 => LoadKind::Bu,
                5 => LoadKind::Hu,
                6 => LoadKind::Wu,
                _ => return Err(()),
            };
            Ok(Op::Load {
                kind,
                rd: rd(raw),
                rs1: rs1(raw),
                imm: imm_i(raw),
            })
        }
        // STORE
        0x23 => {
            let kind = match f3 {
                0 => StoreKind::B,
                1 => StoreKind::H,
                2 => StoreKind::W,
                3 => StoreKind::D,
                _ => return Err(()),
            };
            Ok(Op::Store {
                kind,
                rs1: rs1(raw),
                rs2: rs2(raw),
                imm: imm_s(raw),
            })
        }
        // OP-IMM
        0x13 => {
            let (kind, imm) = match f3 {
                0 => (AluOp::Add, imm_i(raw)),
                2 => (AluOp::Slt, imm_i(raw)),
                3 => (AluOp::Sltu, imm_i(raw)),
                4 => (AluOp::Xor, imm_i(raw)),
                6 => (AluOp::Or, imm_i(raw)),
                7 => (AluOp::And, imm_i(raw)),
                // Shifts: 6-bit shamt (bits 25:20), top 6 bits select the op.
                // Anything else in funct3 1/5 is a Zbb/Zbkb/Zk unary form.
                1 => match raw >> 26 {
                    0x00 => (AluOp::Sll, ((raw >> 20) & 0x3f) as i64),
                    _ => return decode_op_imm_bit(raw),
                },
                5 => match raw >> 26 {
                    0x00 => (AluOp::Srl, ((raw >> 20) & 0x3f) as i64),
                    0x10 => (AluOp::Sra, ((raw >> 20) & 0x3f) as i64),
                    // rori: funct6 0b011000
                    0x18 => {
                        return Ok(Op::BitImm {
                            kind: BitImmOp::Rori,
                            rd: rd(raw),
                            rs1: rs1(raw),
                            imm: (raw >> 20) & 0x3f,
                        })
                    }
                    _ => return decode_op_imm_bit(raw),
                },
                _ => unreachable!(),
            };
            Ok(Op::OpImm {
                kind,
                rd: rd(raw),
                rs1: rs1(raw),
                imm,
            })
        }
        // OP-IMM-32
        0x1b => {
            let (kind, imm) = match f3 {
                0 => (AluOp::Add, imm_i(raw)),
                // Shifts: 5-bit shamt; funct7 must be exact (bit 25 set is illegal).
                1 => match funct7(raw) {
                    0x00 => (AluOp::Sll, ((raw >> 20) & 0x1f) as i64),
                    // clzw/ctzw/cpopw: funct12 0x600/0x601/0x602
                    0x30 => {
                        let kind = match (raw >> 20) & 0x1f {
                            0 => BitUnaryOp::Clzw,
                            1 => BitUnaryOp::Ctzw,
                            2 => BitUnaryOp::Cpopw,
                            _ => return Err(()),
                        };
                        return Ok(Op::BitUnary {
                            kind,
                            rd: rd(raw),
                            rs1: rs1(raw),
                        });
                    }
                    _ => return Err(()),
                },
                5 => match funct7(raw) {
                    0x00 => (AluOp::Srl, ((raw >> 20) & 0x1f) as i64),
                    0x20 => (AluOp::Sra, ((raw >> 20) & 0x1f) as i64),
                    // roriw
                    0x30 => {
                        return Ok(Op::BitImm {
                            kind: BitImmOp::Roriw,
                            rd: rd(raw),
                            rs1: rs1(raw),
                            imm: (raw >> 20) & 0x1f,
                        })
                    }
                    _ => return Err(()),
                },
                _ => return Err(()),
            };
            Ok(Op::OpImm32 {
                kind,
                rd: rd(raw),
                rs1: rs1(raw),
                imm,
            })
        }
        // OP
        0x33 => {
            let kind = match (funct7(raw), f3) {
                (0x00, 0) => AluOp::Add,
                (0x20, 0) => AluOp::Sub,
                (0x00, 1) => AluOp::Sll,
                (0x00, 2) => AluOp::Slt,
                (0x00, 3) => AluOp::Sltu,
                (0x00, 4) => AluOp::Xor,
                (0x00, 5) => AluOp::Srl,
                (0x20, 5) => AluOp::Sra,
                (0x00, 6) => AluOp::Or,
                (0x00, 7) => AluOp::And,
                (0x01, 0) => AluOp::Mul,
                (0x01, 1) => AluOp::Mulh,
                (0x01, 2) => AluOp::Mulhsu,
                (0x01, 3) => AluOp::Mulhu,
                (0x01, 4) => AluOp::Div,
                (0x01, 5) => AluOp::Divu,
                (0x01, 6) => AluOp::Rem,
                (0x01, 7) => AluOp::Remu,
                _ => return decode_op_bit(raw, false),
            };
            Ok(Op::OpReg {
                kind,
                rd: rd(raw),
                rs1: rs1(raw),
                rs2: rs2(raw),
            })
        }
        // OP-32
        0x3b => {
            let kind = match (funct7(raw), f3) {
                (0x00, 0) => AluOp::Add,
                (0x20, 0) => AluOp::Sub,
                (0x00, 1) => AluOp::Sll,
                (0x00, 5) => AluOp::Srl,
                (0x20, 5) => AluOp::Sra,
                (0x01, 0) => AluOp::Mul,
                (0x01, 4) => AluOp::Div,
                (0x01, 5) => AluOp::Divu,
                (0x01, 6) => AluOp::Rem,
                (0x01, 7) => AluOp::Remu,
                _ => return decode_op_bit(raw, true),
            };
            Ok(Op::OpReg32 {
                kind,
                rd: rd(raw),
                rs1: rs1(raw),
                rs2: rs2(raw),
            })
        }
        // MISC-MEM
        0x0f => match f3 {
            0 => Ok(Op::Fence),
            1 => Ok(Op::FenceI),
            _ => Err(()),
        },
        // SYSTEM
        0x73 => match f3 {
            0 => match raw {
                0x0000_0073 => Ok(Op::Ecall),
                0x0010_0073 => Ok(Op::Ebreak),
                0x1020_0073 => Ok(Op::Sret),
                0x3020_0073 => Ok(Op::Mret),
                0x1050_0073 => Ok(Op::Wfi),
                _ if funct7(raw) == 0x09 && rd(raw) == 0 => Ok(Op::SfenceVma {
                    rs1: rs1(raw),
                    rs2: rs2(raw),
                }),
                _ => Err(()),
            },
            4 => Err(()),
            _ => {
                let kind = match f3 {
                    1 => CsrOp::Rw,
                    2 => CsrOp::Rs,
                    3 => CsrOp::Rc,
                    5 => CsrOp::Rwi,
                    6 => CsrOp::Rsi,
                    7 => CsrOp::Rci,
                    _ => unreachable!(),
                };
                Ok(Op::Csr {
                    kind,
                    rd: rd(raw),
                    rs1: rs1(raw),
                    csr: ((raw >> 20) & 0xfff) as u16,
                })
            }
        },
        // AMO
        0x2f => {
            let width = match f3 {
                2 => AmoWidth::W,
                3 => AmoWidth::D,
                _ => return Err(()),
            };
            // funct5 = funct7 without the aq/rl bits (single hart: ignored).
            match funct7(raw) >> 2 {
                0x02 => {
                    if rs2(raw) != 0 {
                        return Err(());
                    }
                    Ok(Op::Lr {
                        width,
                        rd: rd(raw),
                        rs1: rs1(raw),
                    })
                }
                0x03 => Ok(Op::Sc {
                    width,
                    rd: rd(raw),
                    rs1: rs1(raw),
                    rs2: rs2(raw),
                }),
                funct5 => {
                    let op = match funct5 {
                        0x01 => AmoOp::Swap,
                        0x00 => AmoOp::Add,
                        0x04 => AmoOp::Xor,
                        0x0c => AmoOp::And,
                        0x08 => AmoOp::Or,
                        0x10 => AmoOp::Min,
                        0x14 => AmoOp::Max,
                        0x18 => AmoOp::Minu,
                        0x1c => AmoOp::Maxu,
                        _ => return Err(()),
                    };
                    Ok(Op::Amo {
                        op,
                        width,
                        rd: rd(raw),
                        rs1: rs1(raw),
                        rs2: rs2(raw),
                    })
                }
            }
        }
        // LOAD-FP
        0x07 => {
            let sz = match f3 {
                2 => FpSize::S,
                3 => FpSize::D,
                _ => return Err(()),
            };
            Ok(Op::FpLoad {
                sz,
                frd: rd(raw),
                rs1: rs1(raw),
                imm: imm_i(raw),
            })
        }
        // STORE-FP
        0x27 => {
            let sz = match f3 {
                2 => FpSize::S,
                3 => FpSize::D,
                _ => return Err(()),
            };
            Ok(Op::FpStore {
                sz,
                rs1: rs1(raw),
                frs2: rs2(raw),
                imm: imm_s(raw),
            })
        }
        // FMADD / FMSUB / FNMSUB / FNMADD
        0x43 | 0x47 | 0x4b | 0x4f => {
            let sz = match (raw >> 25) & 0x3 {
                0 => FpSize::S,
                1 => FpSize::D,
                _ => return Err(()),
            };
            let op = match opcode {
                0x43 => FpOp::Madd,
                0x47 => FpOp::Msub,
                0x4b => FpOp::Nmsub,
                0x4f => FpOp::Nmadd,
                _ => unreachable!(),
            };
            Ok(Op::Fp {
                op,
                sz,
                rd: rd(raw),
                rs1: rs1(raw),
                rs2: rs2(raw),
                rs3: ((raw >> 27) & 0x1f) as usize,
                rm: f3 as u8,
            })
        }
        // OP-FP
        0x53 => decode_op_fp(raw),
        _ => Err(()),
    }
}

fn decode_op_fp(raw: u32) -> Result<Op, ()> {
    let f7 = funct7(raw);
    let f3 = funct3(raw);
    let fmt = f7 & 0x3;
    // Only single (0) and double (1) precision are supported.
    let sz = match fmt {
        0 => FpSize::S,
        1 => FpSize::D,
        _ => return Err(()),
    };
    let (op, sz) = match f7 >> 2 {
        0x00 => (FpOp::Add, sz),
        0x01 => (FpOp::Sub, sz),
        0x02 => (FpOp::Mul, sz),
        0x03 => (FpOp::Div, sz),
        // FSQRT: rs2 field must be zero.
        0x0b => {
            if rs2(raw) != 0 {
                return Err(());
            }
            (FpOp::Sqrt, sz)
        }
        // FSGNJ / FSGNJN / FSGNJX (funct3 is the selector, not rm)
        0x04 => match f3 {
            0 => (FpOp::Sgnj, sz),
            1 => (FpOp::Sgnjn, sz),
            2 => (FpOp::Sgnjx, sz),
            _ => return Err(()),
        },
        // FMIN / FMAX
        0x05 => match f3 {
            0 => (FpOp::Min, sz),
            1 => (FpOp::Max, sz),
            _ => return Err(()),
        },
        // FCVT.{W,WU,L,LU}.fmt (fp -> int; rs2 field selects)
        0x18 => match rs2(raw) {
            0 => (FpOp::CvtWFmt, sz),
            1 => (FpOp::CvtWuFmt, sz),
            2 => (FpOp::CvtLFmt, sz),
            3 => (FpOp::CvtLuFmt, sz),
            _ => return Err(()),
        },
        // FCVT.fmt.{W,WU,L,LU} (int -> fp; rs2 field selects)
        0x1a => match rs2(raw) {
            0 => (FpOp::CvtFmtW, sz),
            1 => (FpOp::CvtFmtWu, sz),
            2 => (FpOp::CvtFmtL, sz),
            3 => (FpOp::CvtFmtLu, sz),
            _ => return Err(()),
        },
        // FCVT.S.D / FCVT.D.S: `sz` (fmt) is the destination, rs2 the source.
        0x08 => match (fmt, rs2(raw)) {
            (0, 1) => (FpOp::CvtFmtFmt, FpSize::S), // fcvt.s.d
            (1, 0) => (FpOp::CvtFmtFmt, FpSize::D), // fcvt.d.s
            _ => return Err(()),
        },
        // FMV.X.W / FMV.X.D / FCLASS
        0x1c => match (f3, rs2(raw)) {
            (0, 0) => (FpOp::MvXFmt, sz),
            (1, 0) => (FpOp::Class, sz),
            _ => return Err(()),
        },
        // FMV.W.X / FMV.D.X
        0x1e => match (f3, rs2(raw)) {
            (0, 0) => (FpOp::MvFmtX, sz),
            _ => return Err(()),
        },
        // FEQ / FLT / FLE
        0x14 => match f3 {
            2 => (FpOp::Eq, sz),
            1 => (FpOp::Lt, sz),
            0 => (FpOp::Le, sz),
            _ => return Err(()),
        },
        _ => return Err(()),
    };
    Ok(Op::Fp {
        op,
        sz,
        rd: rd(raw),
        rs1: rs1(raw),
        rs2: rs2(raw),
        rs3: 0,
        rm: f3 as u8,
    })
}

// ---------------------------------------------------------------------------
// Compressed (RVC) helpers
// ---------------------------------------------------------------------------

/// rd/rs1 field in bits 11:7 (full register number).
fn c_rd(raw: u16) -> usize {
    ((raw >> 7) & 0x1f) as usize
}

/// rs2 field in bits 6:2 (full register number).
fn c_rs2(raw: u16) -> usize {
    ((raw >> 2) & 0x1f) as usize
}

/// rd'/rs2' field in bits 4:2 (x8..x15).
fn c_reg_low(raw: u16) -> usize {
    (8 + ((raw >> 2) & 0x7)) as usize
}

/// rd'/rs1' field in bits 9:7 (x8..x15).
fn c_reg_high(raw: u16) -> usize {
    (8 + ((raw >> 7) & 0x7)) as usize
}

fn sext(value: u64, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

/// CI-format sign-extended 6-bit immediate: imm[5]=inst[12], imm[4:0]=inst[6:2].
fn c_imm6(raw: u16) -> i64 {
    sext(((((raw >> 12) & 0x1) << 5) | ((raw >> 2) & 0x1f)) as u64, 6)
}

/// CI-format unsigned 6-bit shamt (same bit positions as `c_imm6`).
fn c_shamt6(raw: u16) -> i64 {
    (((((raw >> 12) & 0x1) << 5) | ((raw >> 2) & 0x1f)) as u64) as i64
}

fn decode16(raw: u16) -> Result<Op, ()> {
    if raw == 0 {
        return Err(());
    }
    let f3 = (raw >> 13) & 0x7;
    match raw & 0x3 {
        // ------------------------------------------------------------------
        // Quadrant 0
        // ------------------------------------------------------------------
        0b00 => match f3 {
            // C.ADDI4SPN: addi rd', x2, nzuimm
            0 => {
                // nzuimm[5:4|9:6|2|3] = inst[12:11|10:7|6|5]
                let imm = ((((raw >> 11) & 0x3) << 4)
                    | (((raw >> 7) & 0xf) << 6)
                    | (((raw >> 6) & 0x1) << 2)
                    | (((raw >> 5) & 0x1) << 3)) as i64;
                if imm == 0 {
                    return Err(());
                }
                Ok(Op::OpImm {
                    kind: AluOp::Add,
                    rd: c_reg_low(raw),
                    rs1: 2,
                    imm,
                })
            }
            // C.FLD / C.LD / C.FSD / C.SD: uimm[5:3|7:6] = inst[12:10|6:5]
            1 | 3 | 5 | 7 => {
                let imm = ((((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6)) as i64;
                match f3 {
                    1 => Ok(Op::FpLoad {
                        sz: FpSize::D,
                        frd: c_reg_low(raw),
                        rs1: c_reg_high(raw),
                        imm,
                    }),
                    3 => Ok(Op::Load {
                        kind: LoadKind::D,
                        rd: c_reg_low(raw),
                        rs1: c_reg_high(raw),
                        imm,
                    }),
                    5 => Ok(Op::FpStore {
                        sz: FpSize::D,
                        rs1: c_reg_high(raw),
                        frs2: c_reg_low(raw),
                        imm,
                    }),
                    7 => Ok(Op::Store {
                        kind: StoreKind::D,
                        rs1: c_reg_high(raw),
                        rs2: c_reg_low(raw),
                        imm,
                    }),
                    _ => unreachable!(),
                }
            }
            // C.LW / C.SW: uimm[5:3|2|6] = inst[12:10|6|5]
            2 | 6 => {
                let imm = ((((raw >> 10) & 0x7) << 3)
                    | (((raw >> 6) & 0x1) << 2)
                    | (((raw >> 5) & 0x1) << 6)) as i64;
                match f3 {
                    2 => Ok(Op::Load {
                        kind: LoadKind::W,
                        rd: c_reg_low(raw),
                        rs1: c_reg_high(raw),
                        imm,
                    }),
                    6 => Ok(Op::Store {
                        kind: StoreKind::W,
                        rs1: c_reg_high(raw),
                        rs2: c_reg_low(raw),
                        imm,
                    }),
                    _ => unreachable!(),
                }
            }
            // f3 == 4 is reserved
            _ => Err(()),
        },
        // ------------------------------------------------------------------
        // Quadrant 1
        // ------------------------------------------------------------------
        0b01 => match f3 {
            // C.NOP / C.ADDI: addi rd, rd, imm
            0 => {
                let r = c_rd(raw);
                Ok(Op::OpImm {
                    kind: AluOp::Add,
                    rd: r,
                    rs1: r,
                    imm: c_imm6(raw),
                })
            }
            // C.ADDIW (RV64; rd == 0 reserved): addiw rd, rd, imm
            1 => {
                let r = c_rd(raw);
                if r == 0 {
                    return Err(());
                }
                Ok(Op::OpImm32 {
                    kind: AluOp::Add,
                    rd: r,
                    rs1: r,
                    imm: c_imm6(raw),
                })
            }
            // C.LI: addi rd, x0, imm
            2 => Ok(Op::OpImm {
                kind: AluOp::Add,
                rd: c_rd(raw),
                rs1: 0,
                imm: c_imm6(raw),
            }),
            // C.ADDI16SP / C.LUI
            3 => {
                let r = c_rd(raw);
                if r == 2 {
                    // C.ADDI16SP: nzimm[9|4|6|8:7|5] = inst[12|6|5|4:3|2]
                    let imm = sext(
                        ((((raw >> 12) & 0x1) << 9)
                            | (((raw >> 6) & 0x1) << 4)
                            | (((raw >> 5) & 0x1) << 6)
                            | (((raw >> 3) & 0x3) << 7)
                            | (((raw >> 2) & 0x1) << 5)) as u64,
                        10,
                    );
                    if imm == 0 {
                        return Err(());
                    }
                    Ok(Op::OpImm {
                        kind: AluOp::Add,
                        rd: 2,
                        rs1: 2,
                        imm,
                    })
                } else {
                    // C.LUI: nzimm[17:12] = inst[12|6:2] (rd == 0 is a hint)
                    let imm = c_imm6(raw) << 12;
                    if imm == 0 {
                        return Err(());
                    }
                    Ok(Op::Lui { rd: r, imm })
                }
            }
            // C.SRLI / C.SRAI / C.ANDI / register-register ops
            4 => {
                let r = c_reg_high(raw);
                match (raw >> 10) & 0x3 {
                    0 => Ok(Op::OpImm {
                        kind: AluOp::Srl,
                        rd: r,
                        rs1: r,
                        imm: c_shamt6(raw),
                    }),
                    1 => Ok(Op::OpImm {
                        kind: AluOp::Sra,
                        rd: r,
                        rs1: r,
                        imm: c_shamt6(raw),
                    }),
                    2 => Ok(Op::OpImm {
                        kind: AluOp::And,
                        rd: r,
                        rs1: r,
                        imm: c_imm6(raw),
                    }),
                    _ => {
                        let rs2 = c_reg_low(raw);
                        match ((raw >> 12) & 0x1) << 2 | ((raw >> 5) & 0x3) {
                            0b000 => Ok(Op::OpReg {
                                kind: AluOp::Sub,
                                rd: r,
                                rs1: r,
                                rs2,
                            }),
                            0b001 => Ok(Op::OpReg {
                                kind: AluOp::Xor,
                                rd: r,
                                rs1: r,
                                rs2,
                            }),
                            0b010 => Ok(Op::OpReg {
                                kind: AluOp::Or,
                                rd: r,
                                rs1: r,
                                rs2,
                            }),
                            0b011 => Ok(Op::OpReg {
                                kind: AluOp::And,
                                rd: r,
                                rs1: r,
                                rs2,
                            }),
                            0b100 => Ok(Op::OpReg32 {
                                kind: AluOp::Sub,
                                rd: r,
                                rs1: r,
                                rs2,
                            }),
                            0b101 => Ok(Op::OpReg32 {
                                kind: AluOp::Add,
                                rd: r,
                                rs1: r,
                                rs2,
                            }),
                            _ => Err(()),
                        }
                    }
                }
            }
            // C.J: imm[11|4|9:8|10|6|7|3:1|5] = inst[12|11|10:9|8|7|6|5:3|2]
            5 => {
                let imm = sext(
                    ((((raw >> 12) & 0x1) << 11)
                        | (((raw >> 11) & 0x1) << 4)
                        | (((raw >> 9) & 0x3) << 8)
                        | (((raw >> 8) & 0x1) << 10)
                        | (((raw >> 7) & 0x1) << 6)
                        | (((raw >> 6) & 0x1) << 7)
                        | (((raw >> 3) & 0x7) << 1)
                        | (((raw >> 2) & 0x1) << 5)) as u64,
                    12,
                );
                Ok(Op::Jal { rd: 0, imm })
            }
            // C.BEQZ / C.BNEZ: imm[8|4:3|7:6|2:1|5] = inst[12|11:10|6:5|4:3|2]
            6 | 7 => {
                let imm = sext(
                    ((((raw >> 12) & 0x1) << 8)
                        | (((raw >> 10) & 0x3) << 3)
                        | (((raw >> 5) & 0x3) << 6)
                        | (((raw >> 3) & 0x3) << 1)
                        | (((raw >> 2) & 0x1) << 5)) as u64,
                    9,
                );
                let cond = if f3 == 6 {
                    BranchCond::Eq
                } else {
                    BranchCond::Ne
                };
                Ok(Op::Branch {
                    cond,
                    rs1: c_reg_high(raw),
                    rs2: 0,
                    imm,
                })
            }
            _ => unreachable!(),
        },
        // ------------------------------------------------------------------
        // Quadrant 2
        // ------------------------------------------------------------------
        0b10 => match f3 {
            // C.SLLI: slli rd, rd, shamt
            0 => {
                let r = c_rd(raw);
                Ok(Op::OpImm {
                    kind: AluOp::Sll,
                    rd: r,
                    rs1: r,
                    imm: c_shamt6(raw),
                })
            }
            // C.FLDSP / C.LDSP: uimm[5|4:3|8:6] = inst[12|6:5|4:2]
            1 | 3 => {
                let imm = ((((raw >> 12) & 0x1) << 5)
                    | (((raw >> 5) & 0x3) << 3)
                    | (((raw >> 2) & 0x7) << 6)) as i64;
                if f3 == 1 {
                    Ok(Op::FpLoad {
                        sz: FpSize::D,
                        frd: c_rd(raw),
                        rs1: 2,
                        imm,
                    })
                } else {
                    if c_rd(raw) == 0 {
                        return Err(());
                    }
                    Ok(Op::Load {
                        kind: LoadKind::D,
                        rd: c_rd(raw),
                        rs1: 2,
                        imm,
                    })
                }
            }
            // C.LWSP: uimm[5|4:2|7:6] = inst[12|6:4|3:2]
            2 => {
                if c_rd(raw) == 0 {
                    return Err(());
                }
                let imm = ((((raw >> 12) & 0x1) << 5)
                    | (((raw >> 4) & 0x7) << 2)
                    | (((raw >> 2) & 0x3) << 6)) as i64;
                Ok(Op::Load {
                    kind: LoadKind::W,
                    rd: c_rd(raw),
                    rs1: 2,
                    imm,
                })
            }
            // C.JR / C.MV / C.EBREAK / C.JALR / C.ADD
            4 => {
                let r = c_rd(raw);
                let rs2 = c_rs2(raw);
                if (raw >> 12) & 0x1 == 0 {
                    if rs2 == 0 {
                        // C.JR (rs1 == 0 reserved)
                        if r == 0 {
                            return Err(());
                        }
                        Ok(Op::Jalr {
                            rd: 0,
                            rs1: r,
                            imm: 0,
                        })
                    } else {
                        // C.MV: add rd, x0, rs2
                        Ok(Op::OpReg {
                            kind: AluOp::Add,
                            rd: r,
                            rs1: 0,
                            rs2,
                        })
                    }
                } else if rs2 == 0 {
                    if r == 0 {
                        Ok(Op::Ebreak)
                    } else {
                        // C.JALR: jalr x1, 0(rs1)
                        Ok(Op::Jalr {
                            rd: 1,
                            rs1: r,
                            imm: 0,
                        })
                    }
                } else {
                    // C.ADD: add rd, rd, rs2
                    Ok(Op::OpReg {
                        kind: AluOp::Add,
                        rd: r,
                        rs1: r,
                        rs2,
                    })
                }
            }
            // C.FSDSP / C.SDSP: uimm[5:3|8:6] = inst[12:10|9:7]
            5 | 7 => {
                let imm = ((((raw >> 10) & 0x7) << 3) | (((raw >> 7) & 0x7) << 6)) as i64;
                if f3 == 5 {
                    Ok(Op::FpStore {
                        sz: FpSize::D,
                        rs1: 2,
                        frs2: c_rs2(raw),
                        imm,
                    })
                } else {
                    Ok(Op::Store {
                        kind: StoreKind::D,
                        rs1: 2,
                        rs2: c_rs2(raw),
                        imm,
                    })
                }
            }
            // C.SWSP: uimm[5:2|7:6] = inst[12:9|8:7]
            6 => {
                let imm = ((((raw >> 9) & 0xf) << 2) | (((raw >> 7) & 0x3) << 6)) as i64;
                Ok(Op::Store {
                    kind: StoreKind::W,
                    rs1: 2,
                    rs2: c_rs2(raw),
                    imm,
                })
            }
            _ => unreachable!(),
        },
        // Quadrant 3 is the 32-bit space; `decode` never routes it here.
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a 32-bit instruction, asserting `len == 4`.
    fn d32(raw: u32) -> Op {
        let inst = decode(raw).expect("expected legal 32-bit instruction");
        assert_eq!(inst.len, 4, "raw {raw:#010x} should decode as 4 bytes");
        inst.op
    }

    /// Decode a compressed instruction, asserting `len == 2`.
    fn d16(raw: u16) -> Op {
        let inst = decode(raw as u32).expect("expected legal compressed instruction");
        assert_eq!(inst.len, 2, "raw {raw:#06x} should decode as 2 bytes");
        inst.op
    }

    #[test]
    fn rv64i_upper_and_jumps() {
        // lui x5, 0x12345
        assert_eq!(
            d32(0x1234_52b7),
            Op::Lui {
                rd: 5,
                imm: 0x1234_5000
            }
        );
        // lui x1, 0xfffff (negative when sign-extended)
        assert_eq!(d32(0xffff_f0b7), Op::Lui { rd: 1, imm: -4096 });
        // auipc x1, 1
        assert_eq!(d32(0x0000_1097), Op::Auipc { rd: 1, imm: 0x1000 });
        // jal x1, 8
        assert_eq!(d32(0x0080_00ef), Op::Jal { rd: 1, imm: 8 });
        // j -8 (jal x0, -8)
        assert_eq!(d32(0xff9f_f06f), Op::Jal { rd: 0, imm: -8 });
        // ret (jalr x0, 0(x1))
        assert_eq!(
            d32(0x0000_8067),
            Op::Jalr {
                rd: 0,
                rs1: 1,
                imm: 0
            }
        );
        // jalr x1, -4(x5)
        assert_eq!(
            d32(0xffc2_80e7),
            Op::Jalr {
                rd: 1,
                rs1: 5,
                imm: -4
            }
        );
    }

    #[test]
    fn rv64i_branches() {
        // beq x1, x2, -8
        assert_eq!(
            d32(0xfe20_8ce3),
            Op::Branch {
                cond: BranchCond::Eq,
                rs1: 1,
                rs2: 2,
                imm: -8
            }
        );
        // bne x10, x11, 16
        // imm=16: imm[4:1]=1000 -> bits 11:8
        assert_eq!(
            d32(0x00b5_1863),
            Op::Branch {
                cond: BranchCond::Ne,
                rs1: 10,
                rs2: 11,
                imm: 16
            }
        );
        // bltu x1, x2, 4096: imm[12]=1 impossible for positive... use bgeu x1, x2, -4096
        // imm = -4096: imm[12]=1(bit31), imm[11]=0(bit7), rest 0
        assert_eq!(
            d32(0x8020_f063),
            Op::Branch {
                cond: BranchCond::Geu,
                rs1: 1,
                rs2: 2,
                imm: -4096
            }
        );
    }

    #[test]
    fn rv64i_loads_stores() {
        // ld x1, 0(x2)
        assert_eq!(
            d32(0x0001_3083),
            Op::Load {
                kind: LoadKind::D,
                rd: 1,
                rs1: 2,
                imm: 0
            }
        );
        // lw x10, -4(x2)
        assert_eq!(
            d32(0xffc1_2503),
            Op::Load {
                kind: LoadKind::W,
                rd: 10,
                rs1: 2,
                imm: -4
            }
        );
        // lbu x3, 5(x4)
        assert_eq!(
            d32(0x0052_4183),
            Op::Load {
                kind: LoadKind::Bu,
                rd: 3,
                rs1: 4,
                imm: 5
            }
        );
        // lwu x3, 0(x4)
        assert_eq!(
            d32(0x0002_6183),
            Op::Load {
                kind: LoadKind::Wu,
                rd: 3,
                rs1: 4,
                imm: 0
            }
        );
        // sw x2, 8(x1)
        assert_eq!(
            d32(0x0020_a423),
            Op::Store {
                kind: StoreKind::W,
                rs1: 1,
                rs2: 2,
                imm: 8
            }
        );
        // sd x5, -8(x2)
        assert_eq!(
            d32(0xfe51_3c23),
            Op::Store {
                kind: StoreKind::D,
                rs1: 2,
                rs2: 5,
                imm: -8
            }
        );
    }

    #[test]
    fn rv64i_op_imm() {
        // addi x1, x2, 3
        assert_eq!(
            d32(0x0031_0093),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 1,
                rs1: 2,
                imm: 3
            }
        );
        // addi x1, x0, -1
        assert_eq!(
            d32(0xfff0_0093),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 1,
                rs1: 0,
                imm: -1
            }
        );
        // xori x1, x2, -1
        assert_eq!(
            d32(0xfff1_4093),
            Op::OpImm {
                kind: AluOp::Xor,
                rd: 1,
                rs1: 2,
                imm: -1
            }
        );
        // sltiu x1, x2, 10
        assert_eq!(
            d32(0x00a1_3093),
            Op::OpImm {
                kind: AluOp::Sltu,
                rd: 1,
                rs1: 2,
                imm: 10
            }
        );
        // slli x1, x2, 32 (RV64: 6-bit shamt, bit 25 legal)
        assert_eq!(
            d32(0x0201_1093),
            Op::OpImm {
                kind: AluOp::Sll,
                rd: 1,
                rs1: 2,
                imm: 32
            }
        );
        // srai x1, x2, 63
        assert_eq!(
            d32(0x43f1_5093),
            Op::OpImm {
                kind: AluOp::Sra,
                rd: 1,
                rs1: 2,
                imm: 63
            }
        );
        // srli x1, x2, 1
        assert_eq!(
            d32(0x0011_5093),
            Op::OpImm {
                kind: AluOp::Srl,
                rd: 1,
                rs1: 2,
                imm: 1
            }
        );
    }

    #[test]
    fn rv64i_op_imm32_and_illegal_shamts() {
        // addiw x1, x2, -1
        assert_eq!(
            d32(0xfff1_009b),
            Op::OpImm32 {
                kind: AluOp::Add,
                rd: 1,
                rs1: 2,
                imm: -1
            }
        );
        // slliw x1, x2, 31
        assert_eq!(
            d32(0x01f1_109b),
            Op::OpImm32 {
                kind: AluOp::Sll,
                rd: 1,
                rs1: 2,
                imm: 31
            }
        );
        // sraiw x1, x2, 3
        assert_eq!(
            d32(0x4031_509b),
            Op::OpImm32 {
                kind: AluOp::Sra,
                rd: 1,
                rs1: 2,
                imm: 3
            }
        );
        // slliw/srliw with shamt bit 5 set are illegal on RV64
        assert_eq!(decode(0x0201_109b), Err(())); // slliw shamt=32
        assert_eq!(decode(0x0201_509b), Err(())); // srliw shamt=32
        assert_eq!(decode(0x4201_509b), Err(())); // sraiw shamt=32
    }

    #[test]
    fn rv64i_op_reg() {
        // add x1, x2, x3
        assert_eq!(
            d32(0x0031_00b3),
            Op::OpReg {
                kind: AluOp::Add,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // sub x1, x2, x3
        assert_eq!(
            d32(0x4031_00b3),
            Op::OpReg {
                kind: AluOp::Sub,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // sra x1, x2, x3
        assert_eq!(
            d32(0x4031_50b3),
            Op::OpReg {
                kind: AluOp::Sra,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // and x1, x2, x3
        assert_eq!(
            d32(0x0031_70b3),
            Op::OpReg {
                kind: AluOp::And,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // addw x1, x2, x3
        assert_eq!(
            d32(0x0031_00bb),
            Op::OpReg32 {
                kind: AluOp::Add,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // subw x1, x2, x3
        assert_eq!(
            d32(0x4031_00bb),
            Op::OpReg32 {
                kind: AluOp::Sub,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
    }

    #[test]
    fn m_extension() {
        // mul x1, x2, x3
        assert_eq!(
            d32(0x0231_00b3),
            Op::OpReg {
                kind: AluOp::Mul,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // mulhu x1, x2, x3
        assert_eq!(
            d32(0x0231_30b3),
            Op::OpReg {
                kind: AluOp::Mulhu,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // divu x1, x2, x3
        assert_eq!(
            d32(0x0231_50b3),
            Op::OpReg {
                kind: AluOp::Divu,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // remu x1, x2, x3
        assert_eq!(
            d32(0x0231_70b3),
            Op::OpReg {
                kind: AluOp::Remu,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // divw x1, x2, x3
        assert_eq!(
            d32(0x0231_40bb),
            Op::OpReg32 {
                kind: AluOp::Div,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // remw x1, x2, x3
        assert_eq!(
            d32(0x0231_60bb),
            Op::OpReg32 {
                kind: AluOp::Rem,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // mulw x1, x2, x3
        assert_eq!(
            d32(0x0231_00bb),
            Op::OpReg32 {
                kind: AluOp::Mul,
                rd: 1,
                rs1: 2,
                rs2: 3
            }
        );
        // mulh exists only in OP; OP-32 f3=1 with funct7=1 is illegal
        assert_eq!(decode(0x0231_10bb), Err(()));
    }

    #[test]
    fn system_and_fences() {
        assert_eq!(d32(0x0000_0073), Op::Ecall);
        assert_eq!(d32(0x0010_0073), Op::Ebreak);
        assert_eq!(d32(0x3020_0073), Op::Mret);
        assert_eq!(d32(0x1020_0073), Op::Sret);
        assert_eq!(d32(0x1050_0073), Op::Wfi);
        // sfence.vma x0, x0
        assert_eq!(d32(0x1200_0073), Op::SfenceVma { rs1: 0, rs2: 0 });
        // sfence.vma x1, x2
        assert_eq!(d32(0x1220_8073), Op::SfenceVma { rs1: 1, rs2: 2 });
        // fence iorw, iorw
        assert_eq!(d32(0x0ff0_000f), Op::Fence);
        // fence.i
        assert_eq!(d32(0x0000_100f), Op::FenceI);
    }

    #[test]
    fn zicsr() {
        // csrrw x1, mstatus (0x300), x2
        assert_eq!(
            d32(0x3001_10f3),
            Op::Csr {
                kind: CsrOp::Rw,
                rd: 1,
                rs1: 2,
                csr: 0x300
            }
        );
        // csrrs x5, mcause (0x342), x0 (csrr x5, mcause)
        assert_eq!(
            d32(0x3420_22f3),
            Op::Csr {
                kind: CsrOp::Rs,
                rd: 5,
                rs1: 0,
                csr: 0x342
            }
        );
        // csrrc x1, 0x300, x2
        assert_eq!(
            d32(0x3001_30f3),
            Op::Csr {
                kind: CsrOp::Rc,
                rd: 1,
                rs1: 2,
                csr: 0x300
            }
        );
        // csrrwi x1, 0x305, 31 (zimm in rs1 slot)
        assert_eq!(
            d32(0x305f_d0f3),
            Op::Csr {
                kind: CsrOp::Rwi,
                rd: 1,
                rs1: 31,
                csr: 0x305
            }
        );
        // csrrsi x1, 0x304, 5
        assert_eq!(
            d32(0x3042_e0f3),
            Op::Csr {
                kind: CsrOp::Rsi,
                rd: 1,
                rs1: 5,
                csr: 0x304
            }
        );
        // csrrci x1, 0x304, 5
        assert_eq!(
            d32(0x3042_f0f3),
            Op::Csr {
                kind: CsrOp::Rci,
                rd: 1,
                rs1: 5,
                csr: 0x304
            }
        );
        // SYSTEM funct3=4 is illegal
        assert_eq!(decode(0x3001_40f3), Err(()));
    }

    #[test]
    fn a_extension() {
        // lr.w x5, (x10)
        assert_eq!(
            d32(0x1005_22af),
            Op::Lr {
                width: AmoWidth::W,
                rd: 5,
                rs1: 10
            }
        );
        // lr.d x5, (x10)
        assert_eq!(
            d32(0x1005_32af),
            Op::Lr {
                width: AmoWidth::D,
                rd: 5,
                rs1: 10
            }
        );
        // lr.w with rs2 != 0 is illegal
        assert_eq!(decode(0x1065_22af), Err(()));
        // sc.d x5, x6, (x7)
        assert_eq!(
            d32(0x1863_b2af),
            Op::Sc {
                width: AmoWidth::D,
                rd: 5,
                rs1: 7,
                rs2: 6
            }
        );
        // amoadd.w x5, x6, (x7)
        assert_eq!(
            d32(0x0063_a2af),
            Op::Amo {
                op: AmoOp::Add,
                width: AmoWidth::W,
                rd: 5,
                rs1: 7,
                rs2: 6
            }
        );
        // amoswap.d.aqrl x5, x6, (x7) — aq/rl bits ignored
        assert_eq!(
            d32(0x0e63_b2af),
            Op::Amo {
                op: AmoOp::Swap,
                width: AmoWidth::D,
                rd: 5,
                rs1: 7,
                rs2: 6
            }
        );
        // amomaxu.w x5, x6, (x7)
        assert_eq!(
            d32(0xe063_a2af),
            Op::Amo {
                op: AmoOp::Maxu,
                width: AmoWidth::W,
                rd: 5,
                rs1: 7,
                rs2: 6
            }
        );
        // amo funct3 must be 2 or 3
        assert_eq!(decode(0x0063_92af), Err(()));
    }

    #[test]
    fn fp_load_store() {
        // fld f1, 16(x2)
        assert_eq!(
            d32(0x0101_3087),
            Op::FpLoad {
                sz: FpSize::D,
                frd: 1,
                rs1: 2,
                imm: 16
            }
        );
        // flw f1, -4(x2)
        assert_eq!(
            d32(0xffc1_2087),
            Op::FpLoad {
                sz: FpSize::S,
                frd: 1,
                rs1: 2,
                imm: -4
            }
        );
        // fsd f1, 8(x2)
        assert_eq!(
            d32(0x0011_3427),
            Op::FpStore {
                sz: FpSize::D,
                rs1: 2,
                frs2: 1,
                imm: 8
            }
        );
        // fp load/store funct3 outside {2,3} is illegal
        assert_eq!(decode(0x0001_1087), Err(()));
        assert_eq!(decode(0x0011_1427), Err(()));
    }

    #[test]
    fn fp_ops() {
        // fadd.d f1, f2, f3 (rm=dyn)
        assert_eq!(
            d32(0x0231_70d3),
            Op::Fp {
                op: FpOp::Add,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 3,
                rs3: 0,
                rm: 7
            }
        );
        // fmadd.s f1, f2, f3, f4 (rm=dyn)
        assert_eq!(
            d32(0x2031_70c3),
            Op::Fp {
                op: FpOp::Madd,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 3,
                rs3: 4,
                rm: 7
            }
        );
        // fnmadd.d f1, f2, f3, f4
        assert_eq!(
            d32(0x2231_70cf),
            Op::Fp {
                op: FpOp::Nmadd,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 3,
                rs3: 4,
                rm: 7
            }
        );
        // fsgnj.s f1, f2, f2 (fmv.s f1, f2)
        assert_eq!(
            d32(0x2021_00d3),
            Op::Fp {
                op: FpOp::Sgnj,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 2,
                rs3: 0,
                rm: 0
            }
        );
        // fsqrt.d f1, f2
        assert_eq!(
            d32(0x5a01_70d3),
            Op::Fp {
                op: FpOp::Sqrt,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 0,
                rs3: 0,
                rm: 7
            }
        );
        // fcvt.w.s x1, f2, rtz
        assert_eq!(
            d32(0xc001_10d3),
            Op::Fp {
                op: FpOp::CvtWFmt,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 0,
                rs3: 0,
                rm: 1
            }
        );
        // fcvt.d.l f1, x2
        assert_eq!(
            d32(0xd221_00d3),
            Op::Fp {
                op: FpOp::CvtFmtL,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 2,
                rs3: 0,
                rm: 0
            }
        );
        // fcvt.d.s f1, f2 (destination format D)
        assert_eq!(
            d32(0x4201_00d3),
            Op::Fp {
                op: FpOp::CvtFmtFmt,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 0,
                rs3: 0,
                rm: 0
            }
        );
        // fcvt.s.d f1, f2, dyn (destination format S)
        assert_eq!(
            d32(0x4011_70d3),
            Op::Fp {
                op: FpOp::CvtFmtFmt,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 1,
                rs3: 0,
                rm: 7
            }
        );
        // fmv.x.d x1, f2
        assert_eq!(
            d32(0xe201_00d3),
            Op::Fp {
                op: FpOp::MvXFmt,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 0,
                rs3: 0,
                rm: 0
            }
        );
        // fmv.w.x f1, x2
        assert_eq!(
            d32(0xf001_00d3),
            Op::Fp {
                op: FpOp::MvFmtX,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 0,
                rs3: 0,
                rm: 0
            }
        );
        // fclass.s x1, f2
        assert_eq!(
            d32(0xe001_10d3),
            Op::Fp {
                op: FpOp::Class,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 0,
                rs3: 0,
                rm: 1
            }
        );
        // feq.d x1, f2, f3
        assert_eq!(
            d32(0xa231_20d3),
            Op::Fp {
                op: FpOp::Eq,
                sz: FpSize::D,
                rd: 1,
                rs1: 2,
                rs2: 3,
                rs3: 0,
                rm: 2
            }
        );
        // flt.s x1, f2, f3
        assert_eq!(
            d32(0xa031_10d3),
            Op::Fp {
                op: FpOp::Lt,
                sz: FpSize::S,
                rd: 1,
                rs1: 2,
                rs2: 3,
                rs3: 0,
                rm: 1
            }
        );
        // illegal fmt (quad) on fadd
        assert_eq!(decode(0x0631_70d3), Err(()));
        // fsgnj with funct3=3 is illegal
        assert_eq!(decode(0x2021_30d3), Err(()));
        // fcvt.s.s (funct7=0x20, rs2=0) is illegal
        assert_eq!(decode(0x4001_00d3), Err(()));
    }

    #[test]
    fn compressed_quadrant0() {
        // c.addi4spn a0, sp, 4 -> addi x10, x2, 4
        assert_eq!(
            d16(0x0048),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 10,
                rs1: 2,
                imm: 4
            }
        );
        // c.addi4spn a0, sp, 1020 (max imm: uimm[9:2] all ones)
        assert_eq!(
            d16(0x1fe8),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 10,
                rs1: 2,
                imm: 1020
            }
        );
        // c.addi4spn with imm == 0 is reserved
        assert_eq!(decode(0x0008), Err(()));
        // c.lw a1, 4(a0)
        assert_eq!(
            d16(0x414c),
            Op::Load {
                kind: LoadKind::W,
                rd: 11,
                rs1: 10,
                imm: 4
            }
        );
        // c.ld a1, 8(a0)
        assert_eq!(
            d16(0x650c),
            Op::Load {
                kind: LoadKind::D,
                rd: 11,
                rs1: 10,
                imm: 8
            }
        );
        // c.fld fa1, 8(a0)
        assert_eq!(
            d16(0x250c),
            Op::FpLoad {
                sz: FpSize::D,
                frd: 11,
                rs1: 10,
                imm: 8
            }
        );
        // c.sw a1, 4(a0)
        assert_eq!(
            d16(0xc14c),
            Op::Store {
                kind: StoreKind::W,
                rs1: 10,
                rs2: 11,
                imm: 4
            }
        );
        // c.sd a1, 8(a0)
        assert_eq!(
            d16(0xe50c),
            Op::Store {
                kind: StoreKind::D,
                rs1: 10,
                rs2: 11,
                imm: 8
            }
        );
        // c.fsd fa1, 8(a0)
        assert_eq!(
            d16(0xa50c),
            Op::FpStore {
                sz: FpSize::D,
                rs1: 10,
                frs2: 11,
                imm: 8
            }
        );
        // quadrant 0 funct3=4 reserved
        assert_eq!(decode(0x8000), Err(()));
    }

    #[test]
    fn compressed_quadrant1() {
        // c.nop
        assert_eq!(
            d16(0x0001),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 0,
                rs1: 0,
                imm: 0
            }
        );
        // c.addi a0, -1
        assert_eq!(
            d16(0x157d),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 10,
                rs1: 10,
                imm: -1
            }
        );
        // c.addiw a0, -1
        assert_eq!(
            d16(0x357d),
            Op::OpImm32 {
                kind: AluOp::Add,
                rd: 10,
                rs1: 10,
                imm: -1
            }
        );
        // c.addiw with rd == 0 is reserved (would be c.jal on RV32)
        assert_eq!(decode(0x3001_u32), Err(()));
        // c.li a0, 0
        assert_eq!(
            d16(0x4501),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 10,
                rs1: 0,
                imm: 0
            }
        );
        // c.li a0, -32
        assert_eq!(
            d16(0x5501),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 10,
                rs1: 0,
                imm: -32
            }
        );
        // c.addi16sp 16
        assert_eq!(
            d16(0x6141),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 2,
                rs1: 2,
                imm: 16
            }
        );
        // c.addi16sp -64
        assert_eq!(
            d16(0x7139),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 2,
                rs1: 2,
                imm: -64
            }
        );
        // c.addi16sp -32
        assert_eq!(
            d16(0x713d),
            Op::OpImm {
                kind: AluOp::Add,
                rd: 2,
                rs1: 2,
                imm: -32
            }
        );
        // c.addi16sp with imm == 0 is reserved
        assert_eq!(decode(0x6101_u32), Err(()));
        // c.lui a0, 1
        assert_eq!(
            d16(0x6505),
            Op::Lui {
                rd: 10,
                imm: 0x1000
            }
        );
        // c.lui a0, 0xfffe1 (negative: imm6 = -31)
        assert_eq!(
            d16(0x7505),
            Op::Lui {
                rd: 10,
                imm: -31 << 12
            }
        );
        // c.lui with imm == 0 is reserved
        assert_eq!(decode(0x6501_u32), Err(()));
        // c.srli a0, 2
        assert_eq!(
            d16(0x8109),
            Op::OpImm {
                kind: AluOp::Srl,
                rd: 10,
                rs1: 10,
                imm: 2
            }
        );
        // c.srli a0, 32 (RV64 6-bit shamt)
        assert_eq!(
            d16(0x9101),
            Op::OpImm {
                kind: AluOp::Srl,
                rd: 10,
                rs1: 10,
                imm: 32
            }
        );
        // c.srai a0, 2
        assert_eq!(
            d16(0x8509),
            Op::OpImm {
                kind: AluOp::Sra,
                rd: 10,
                rs1: 10,
                imm: 2
            }
        );
        // c.andi a0, -1
        assert_eq!(
            d16(0x997d),
            Op::OpImm {
                kind: AluOp::And,
                rd: 10,
                rs1: 10,
                imm: -1
            }
        );
        // c.sub a0, a1
        assert_eq!(
            d16(0x8d0d),
            Op::OpReg {
                kind: AluOp::Sub,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // c.xor a0, a1
        assert_eq!(
            d16(0x8d2d),
            Op::OpReg {
                kind: AluOp::Xor,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // c.or a0, a1
        assert_eq!(
            d16(0x8d4d),
            Op::OpReg {
                kind: AluOp::Or,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // c.and a0, a1
        assert_eq!(
            d16(0x8d6d),
            Op::OpReg {
                kind: AluOp::And,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // c.subw a0, a1
        assert_eq!(
            d16(0x9d0d),
            Op::OpReg32 {
                kind: AluOp::Sub,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // c.addw a0, a1
        assert_eq!(
            d16(0x9d2d),
            Op::OpReg32 {
                kind: AluOp::Add,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // reserved bit12=1, bits6:5 in {2,3}
        assert_eq!(decode(0x9d4d_u32), Err(()));
        // c.j 0
        assert_eq!(d16(0xa001), Op::Jal { rd: 0, imm: 0 });
        // c.j 8
        assert_eq!(d16(0xa021), Op::Jal { rd: 0, imm: 8 });
        // c.j -4
        assert_eq!(d16(0xbff5), Op::Jal { rd: 0, imm: -4 });
        // c.beqz a0, -4
        assert_eq!(
            d16(0xdd75),
            Op::Branch {
                cond: BranchCond::Eq,
                rs1: 10,
                rs2: 0,
                imm: -4
            }
        );
        // c.bnez a0, 8
        // imm=8: imm[4:3]=01 -> inst[11:10]=01
        assert_eq!(
            d16(0xe501),
            Op::Branch {
                cond: BranchCond::Ne,
                rs1: 10,
                rs2: 0,
                imm: 8
            }
        );
    }

    #[test]
    fn compressed_quadrant2() {
        // c.slli a0, 32 (RV64 6-bit shamt)
        assert_eq!(
            d16(0x1502),
            Op::OpImm {
                kind: AluOp::Sll,
                rd: 10,
                rs1: 10,
                imm: 32
            }
        );
        // c.slli a0, 1
        assert_eq!(
            d16(0x0506),
            Op::OpImm {
                kind: AluOp::Sll,
                rd: 10,
                rs1: 10,
                imm: 1
            }
        );
        // c.lwsp a0, 4(sp)
        assert_eq!(
            d16(0x4512),
            Op::Load {
                kind: LoadKind::W,
                rd: 10,
                rs1: 2,
                imm: 4
            }
        );
        // c.lwsp with rd == 0 is reserved
        assert_eq!(decode(0x4012_u32), Err(()));
        // c.ldsp a0, 8(sp)
        assert_eq!(
            d16(0x6522),
            Op::Load {
                kind: LoadKind::D,
                rd: 10,
                rs1: 2,
                imm: 8
            }
        );
        // c.ldsp a0, 256(sp): uimm[8]=1 -> inst[4]
        assert_eq!(
            d16(0x6512),
            Op::Load {
                kind: LoadKind::D,
                rd: 10,
                rs1: 2,
                imm: 256
            }
        );
        // c.ldsp with rd == 0 is reserved
        assert_eq!(decode(0x6022_u32), Err(()));
        // c.fldsp fa0, 8(sp)
        assert_eq!(
            d16(0x2522),
            Op::FpLoad {
                sz: FpSize::D,
                frd: 10,
                rs1: 2,
                imm: 8
            }
        );
        // c.jr ra (ret)
        assert_eq!(
            d16(0x8082),
            Op::Jalr {
                rd: 0,
                rs1: 1,
                imm: 0
            }
        );
        // c.jr with rs1 == 0 is reserved
        assert_eq!(decode(0x8002_u32), Err(()));
        // c.mv a0, a1
        assert_eq!(
            d16(0x852e),
            Op::OpReg {
                kind: AluOp::Add,
                rd: 10,
                rs1: 0,
                rs2: 11
            }
        );
        // c.ebreak
        assert_eq!(d16(0x9002), Op::Ebreak);
        // c.jalr a0
        assert_eq!(
            d16(0x9502),
            Op::Jalr {
                rd: 1,
                rs1: 10,
                imm: 0
            }
        );
        // c.add a0, a1
        assert_eq!(
            d16(0x952e),
            Op::OpReg {
                kind: AluOp::Add,
                rd: 10,
                rs1: 10,
                rs2: 11
            }
        );
        // c.swsp a0, 4(sp)
        assert_eq!(
            d16(0xc22a),
            Op::Store {
                kind: StoreKind::W,
                rs1: 2,
                rs2: 10,
                imm: 4
            }
        );
        // c.sdsp a0, 8(sp)
        assert_eq!(
            d16(0xe42a),
            Op::Store {
                kind: StoreKind::D,
                rs1: 2,
                rs2: 10,
                imm: 8
            }
        );
        // c.sdsp a0, 64(sp): uimm[6]=1 -> inst[7]
        assert_eq!(
            d16(0xe0aa),
            Op::Store {
                kind: StoreKind::D,
                rs1: 2,
                rs2: 10,
                imm: 64
            }
        );
        // c.fsdsp fa0, 8(sp)
        assert_eq!(
            d16(0xa42a),
            Op::FpStore {
                sz: FpSize::D,
                rs1: 2,
                frs2: 10,
                imm: 8
            }
        );
    }

    #[test]
    fn illegal_encodings() {
        // All-zero 16-bit and 32-bit patterns are defined illegal.
        assert_eq!(decode(0x0000_0000), Err(()));
        // All-ones is illegal (opcode 0x7f is unused).
        assert_eq!(decode(0xffff_ffff), Err(()));
        // Unused major opcode.
        assert_eq!(decode(0x0000_002b), Err(()));
        // Branch funct3 = 2 is illegal.
        assert_eq!(decode(0x0020_a063), Err(()));
        // JALR funct3 != 0 is illegal.
        assert_eq!(decode(0x0000_9067), Err(()));
        // Store funct3 = 4 is illegal.
        assert_eq!(decode(0x0020_c023), Err(()));
        // SRLI with garbage in the top bits.
        assert_eq!(decode(0x8001_5093), Err(()));
    }

    #[test]
    fn lengths() {
        assert_eq!(decode(0x0031_0093).unwrap().len, 4);
        assert_eq!(decode(0x0001_u32).unwrap().len, 2);
        // High half of the fetch window is ignored for compressed decode.
        assert_eq!(decode(0xffff_0001).unwrap().len, 2);
        assert_eq!(
            decode(0xffff_0001).unwrap().op,
            Op::OpImm {
                kind: AluOp::Add,
                rd: 0,
                rs1: 0,
                imm: 0
            }
        );
    }

    // -----------------------------------------------------------------
    // Zbb / Zbc / Zbkb / Zknd / Zkne / Zknh
    //
    // Every raw word below was produced by
    //   llvm-mc -triple=riscv64 -mattr=+zbb,+zbc,+zbkb,+zknd,+zkne,+zknh
    //           -show-encoding
    // (LLVM 21.1.8) for `<mnemonic> t0, t1, t2` — that is rd=x5, rs1=x6,
    // rs2=x7 — and cross-checked against riscv-opcodes `extensions/*`.
    // -----------------------------------------------------------------

    fn bit_rr(kind: BitOp) -> Op {
        Op::Bit {
            kind,
            rd: 5,
            rs1: 6,
            rs2: 7,
        }
    }

    fn bit_un(kind: BitUnaryOp) -> Op {
        Op::BitUnary {
            kind,
            rd: 5,
            rs1: 6,
        }
    }

    #[test]
    fn zbb_zbkb_register_register() {
        assert_eq!(d32(0x4073_72b3), bit_rr(BitOp::Andn));
        assert_eq!(d32(0x4073_62b3), bit_rr(BitOp::Orn));
        assert_eq!(d32(0x4073_42b3), bit_rr(BitOp::Xnor));
        assert_eq!(d32(0x0a73_62b3), bit_rr(BitOp::Max));
        assert_eq!(d32(0x0a73_72b3), bit_rr(BitOp::Maxu));
        assert_eq!(d32(0x0a73_42b3), bit_rr(BitOp::Min));
        assert_eq!(d32(0x0a73_52b3), bit_rr(BitOp::Minu));
        assert_eq!(d32(0x6073_12b3), bit_rr(BitOp::Rol));
        assert_eq!(d32(0x6073_52b3), bit_rr(BitOp::Ror));
        assert_eq!(d32(0x6073_12bb), bit_rr(BitOp::Rolw));
        assert_eq!(d32(0x6073_52bb), bit_rr(BitOp::Rorw));
        assert_eq!(d32(0x0873_42b3), bit_rr(BitOp::Pack));
        assert_eq!(d32(0x0873_72b3), bit_rr(BitOp::Packh));
        assert_eq!(d32(0x0873_42bb), bit_rr(BitOp::Packw));
        // `zext.h t0, t1` is the `packw t0, t1, x0` encoding.
        assert_eq!(
            d32(0x0803_42bb),
            Op::Bit {
                kind: BitOp::Packw,
                rd: 5,
                rs1: 6,
                rs2: 0
            }
        );
    }

    #[test]
    fn zbb_zbkb_unary_and_rotate_immediate() {
        assert_eq!(d32(0x6003_1293), bit_un(BitUnaryOp::Clz));
        assert_eq!(d32(0x6013_1293), bit_un(BitUnaryOp::Ctz));
        assert_eq!(d32(0x6023_1293), bit_un(BitUnaryOp::Cpop));
        assert_eq!(d32(0x6003_129b), bit_un(BitUnaryOp::Clzw));
        assert_eq!(d32(0x6013_129b), bit_un(BitUnaryOp::Ctzw));
        assert_eq!(d32(0x6023_129b), bit_un(BitUnaryOp::Cpopw));
        assert_eq!(d32(0x6043_1293), bit_un(BitUnaryOp::SextB));
        assert_eq!(d32(0x6053_1293), bit_un(BitUnaryOp::SextH));
        assert_eq!(d32(0x2873_5293), bit_un(BitUnaryOp::OrcB));
        assert_eq!(d32(0x6b83_5293), bit_un(BitUnaryOp::Rev8));
        assert_eq!(d32(0x6873_5293), bit_un(BitUnaryOp::Brev8));

        for (raw, shamt) in [(0x6003_5293u32, 0u32), (0x6013_5293, 1), (0x63f3_5293, 63)] {
            assert_eq!(
                d32(raw),
                Op::BitImm {
                    kind: BitImmOp::Rori,
                    rd: 5,
                    rs1: 6,
                    imm: shamt
                }
            );
        }
        for (raw, shamt) in [(0x6003_529bu32, 0u32), (0x6013_529b, 1), (0x61f3_529b, 31)] {
            assert_eq!(
                d32(raw),
                Op::BitImm {
                    kind: BitImmOp::Roriw,
                    rd: 5,
                    rs1: 6,
                    imm: shamt
                }
            );
        }
    }

    #[test]
    fn zbc_carryless_multiply() {
        assert_eq!(d32(0x0a73_12b3), bit_rr(BitOp::Clmul));
        assert_eq!(d32(0x0a73_32b3), bit_rr(BitOp::Clmulh));
        assert_eq!(d32(0x0a73_22b3), bit_rr(BitOp::Clmulr));
    }

    #[test]
    fn zknd_zkne_aes() {
        assert_eq!(d32(0x3273_02b3), bit_rr(BitOp::Aes64es));
        assert_eq!(d32(0x3673_02b3), bit_rr(BitOp::Aes64esm));
        assert_eq!(d32(0x3a73_02b3), bit_rr(BitOp::Aes64ds));
        assert_eq!(d32(0x3e73_02b3), bit_rr(BitOp::Aes64dsm));
        assert_eq!(d32(0x7e73_02b3), bit_rr(BitOp::Aes64ks2));
        assert_eq!(d32(0x3003_1293), bit_un(BitUnaryOp::Aes64im));

        // aes64ks1i for every legal rnum 0..=10.
        for rnum in 0..=10u32 {
            let raw = 0x3103_1293 | (rnum << 20);
            assert_eq!(
                d32(raw),
                Op::BitImm {
                    kind: BitImmOp::Aes64ks1i,
                    rd: 5,
                    rs1: 6,
                    imm: rnum
                }
            );
        }
        // rnum 0xB..0xF are reserved and must not decode.
        for rnum in 11..=15u32 {
            assert_eq!(decode(0x3103_1293 | (rnum << 20)), Err(()));
        }
    }

    #[test]
    fn zknh_sha() {
        assert_eq!(d32(0x1003_1293), bit_un(BitUnaryOp::Sha256sum0));
        assert_eq!(d32(0x1013_1293), bit_un(BitUnaryOp::Sha256sum1));
        assert_eq!(d32(0x1023_1293), bit_un(BitUnaryOp::Sha256sig0));
        assert_eq!(d32(0x1033_1293), bit_un(BitUnaryOp::Sha256sig1));
        assert_eq!(d32(0x1043_1293), bit_un(BitUnaryOp::Sha512sum0));
        assert_eq!(d32(0x1053_1293), bit_un(BitUnaryOp::Sha512sum1));
        assert_eq!(d32(0x1063_1293), bit_un(BitUnaryOp::Sha512sig0));
        assert_eq!(d32(0x1073_1293), bit_un(BitUnaryOp::Sha512sig1));
    }

    /// Encodings adjacent to the new ones, which are still reserved.
    #[test]
    fn bitmanip_neighbours_stay_illegal() {
        // Zbs (bset/bclr/binv/bext) is not implemented.
        assert_eq!(decode(0x2873_12b3), Err(())); // bset t0, t1, t2
        assert_eq!(decode(0x4873_12b3), Err(())); // bclr t0, t1, t2
                                                  // Zba (sh1add/sh2add/sh3add) is not implemented.
        assert_eq!(decode(0x2073_22b3), Err(())); // sh1add t0, t1, t2
                                                  // Zbkx xperm4/xperm8 are not implemented.
        assert_eq!(decode(0x2873_22b3), Err(())); // xperm4 t0, t1, t2
        assert_eq!(decode(0x2873_42b3), Err(())); // xperm8 t0, t1, t2
                                                  // Unassigned funct12 in the Zbb unary slot.
        assert_eq!(decode(0x6033_1293), Err(())); // funct12 0x603
        assert_eq!(decode(0x6063_1293), Err(())); // funct12 0x606
                                                  // sha512 32-bit-only forms do not exist on RV64.
        assert_eq!(decode(0x1083_1293), Err(())); // funct12 0x108
    }
}
