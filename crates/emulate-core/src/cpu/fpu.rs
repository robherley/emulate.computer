//! F/D extension execution.
//!
//! Invariants worth remembering: every FP instruction, loads and moves
//! included, is illegal when mstatus.FS is Off, and anything writing an FP
//! register or fflags sets FS = Dirty. f32 values are NaN-boxed in the 64-bit
//! registers, and a non-boxed value reads as the canonical NaN.
//!
//! Arithmetic goes through `rustc_apfloat` for target-independent, correctly
//! rounded results and exact IEEE exception flags.

use super::csr;
use super::decode::{FpOp, FpSize, Op};
use super::Cpu;
use crate::bus::Bus;
use crate::trap::Exception;
use rustc_apfloat::ieee::{Double, IeeeFloat, Quad, Semantics, Single};
use rustc_apfloat::{Float, FloatConvert, Round, Status, StatusAnd};
use std::cmp::Ordering;

// fflags bits.
const NX: u64 = 1 << 0;
const UF: u64 = 1 << 1;
const OF: u64 = 1 << 2;
const DZ: u64 = 1 << 3;
const NV: u64 = 1 << 4;

const NAN_BOX: u64 = 0xFFFF_FFFF_0000_0000;
const CANONICAL_NAN_F32: u32 = 0x7FC0_0000;
const CANONICAL_NAN_F64: u64 = 0x7FF8_0000_0000_0000;

#[inline]
fn nan_box(bits: u32) -> u64 {
    NAN_BOX | bits as u64
}

/// Raw low 32 bits if properly boxed, else the canonical NaN pattern.
#[inline]
fn unbox_bits(v: u64) -> u32 {
    if v & NAN_BOX == NAN_BOX {
        v as u32
    } else {
        CANONICAL_NAN_F32
    }
}

#[inline]
fn unbox_f32(v: u64) -> f32 {
    f32::from_bits(unbox_bits(v))
}

#[inline]
fn is_snan_f32(x: f32) -> bool {
    x.is_nan() && x.to_bits() & 0x0040_0000 == 0
}

#[inline]
fn is_snan_f64(x: f64) -> bool {
    x.is_nan() && x.to_bits() & 0x0008_0000_0000_0000 == 0
}

/// OR flags into fcsr; any fflags change dirties FS.
fn set_fflags(cpu: &mut Cpu, flags: u64) {
    if flags != 0 {
        let f = cpu.csr.load_raw(csr::FCSR);
        cpu.csr.store_raw(csr::FCSR, f | (flags & 0x1F));
        cpu.csr.set_fs_dirty();
    }
}

/// Write an f32 computational result (canonicalizing NaN) NaN-boxed.
fn write_f32(cpu: &mut Cpu, rd: usize, v: f32) {
    let bits = if v.is_nan() {
        CANONICAL_NAN_F32
    } else {
        v.to_bits()
    };
    cpu.fregs[rd] = nan_box(bits);
    cpu.csr.set_fs_dirty();
}

/// Write raw f32 bits (sgnj/fmv — no NaN canonicalization) NaN-boxed.
fn write_f32_raw(cpu: &mut Cpu, rd: usize, bits: u32) {
    cpu.fregs[rd] = nan_box(bits);
    cpu.csr.set_fs_dirty();
}

/// Write an f64 computational result (canonicalizing NaN).
fn write_f64(cpu: &mut Cpu, rd: usize, v: f64) {
    let bits = if v.is_nan() {
        CANONICAL_NAN_F64
    } else {
        v.to_bits()
    };
    cpu.fregs[rd] = bits;
    cpu.csr.set_fs_dirty();
}

fn write_f64_raw(cpu: &mut Cpu, rd: usize, bits: u64) {
    cpu.fregs[rd] = bits;
    cpu.csr.set_fs_dirty();
}

/// Validate the rounding-mode field and resolve dynamic mode via frm.
/// Returns the effective mode (0..=4).
fn resolve_rm(cpu: &Cpu, raw: u32, rm: u8) -> Result<u8, Exception> {
    let eff = if rm == 7 {
        cpu.csr.load_raw(csr::FRM) as u8
    } else {
        rm
    };
    if rm == 5 || rm == 6 || eff > 4 {
        return Err(Exception::IllegalInstruction(raw as u64));
    }
    Ok(eff)
}

fn ap_round(rm: u8) -> Round {
    match rm {
        0 => Round::NearestTiesToEven,
        1 => Round::TowardZero,
        2 => Round::TowardNegative,
        3 => Round::TowardPositive,
        4 => Round::NearestTiesToAway,
        _ => unreachable!("rm validated by resolve_rm"),
    }
}

fn ap_fflags(status: Status) -> u64 {
    let mut flags = 0;
    if status.contains(Status::INVALID_OP) {
        flags |= NV;
    }
    if status.contains(Status::DIV_BY_ZERO) {
        flags |= DZ;
    }
    if status.contains(Status::OVERFLOW) {
        flags |= OF;
    }
    if status.contains(Status::UNDERFLOW) {
        flags |= UF;
    }
    if status.contains(Status::INEXACT) {
        flags |= NX;
    }
    flags
}

fn adjusted_ap_status<F: Float>(result: F, mut status: Status, overflowed: bool) -> Status {
    // Overflow is recomputed at an unbounded exponent; tininess is repaired
    // at the normal/subnormal boundary before reaching this function.
    status.remove(Status::OVERFLOW);
    if status.contains(Status::INEXACT) {
        if overflowed {
            status |= Status::OVERFLOW;
        } else if result.is_denormal() || result.is_zero() {
            status |= Status::UNDERFLOW;
        }
    }
    status
}

// Keep destination precision but enough exponent range for all F/D operations.
struct Extended<const PRECISION: usize>;
impl<const PRECISION: usize> Semantics for Extended<PRECISION> {
    const BITS: usize = PRECISION + 15;
    const EXP_BITS: usize = 15;
}
type ExtendedSingle = IeeeFloat<Extended<24>>;
type ExtendedDouble = IeeeFloat<Extended<53>>;

fn extended<F: Float + FloatConvert<E>, E: Float>(value: F) -> E {
    value.convert(&mut false).value
}

fn arithmetic<F: Float>(op: FpOp, a: F, b: F, c: F, round: Round) -> StatusAnd<F> {
    match op {
        FpOp::Add => a.add_r(b, round),
        FpOp::Sub => a.sub_r(b, round),
        FpOp::Mul => a.mul_r(b, round),
        FpOp::Div => a.div_r(b, round),
        FpOp::Madd => a.mul_add_r(b, c, round),
        FpOp::Msub => a.mul_add_r(b, -c, round),
        FpOp::Nmsub => (-a).mul_add_r(b, c, round),
        FpOp::Nmadd => (-a).mul_add_r(b, -c, round),
        _ => unreachable!("arithmetic operation"),
    }
}

fn repair_tininess<F: Float + FloatConvert<E>, E: Float>(
    mut result: StatusAnd<F>,
    unbounded: impl FnOnce() -> E,
) -> StatusAnd<F> {
    // After-rounding tininess uses destination precision BEFORE clamping the
    // exponent. A tiny value can subsequently round up to minimum normal.
    if result.status.contains(Status::INEXACT)
        && result.value.abs() == F::smallest_normalized()
        && unbounded().abs() < extended::<F, E>(F::smallest_normalized())
    {
        result.status |= Status::UNDERFLOW;
    }
    result
}

fn write_ap_single(cpu: &mut Cpu, rd: usize, result: StatusAnd<Single>, overflowed: bool) {
    set_fflags(
        cpu,
        ap_fflags(adjusted_ap_status(result.value, result.status, overflowed)),
    );
    write_f32(cpu, rd, f32::from_bits(result.value.to_bits() as u32));
}

fn write_ap_double(cpu: &mut Cpu, rd: usize, result: StatusAnd<Double>, overflowed: bool) {
    set_fflags(
        cpu,
        ap_fflags(adjusted_ap_status(result.value, result.status, overflowed)),
    );
    write_f64(cpu, rd, f64::from_bits(result.value.to_bits() as u64));
}

fn single_wide(value: Single) -> Double {
    let mut loses_info = false;
    value.convert(&mut loses_info).value
}

fn double_wide(value: Double) -> Quad {
    let mut loses_info = false;
    value.convert(&mut loses_info).value
}

fn single_wide_overflows(value: Double) -> bool {
    // 2^128 is one single-precision ULP above the largest finite value.
    value.abs() >= Double::from_bits(0x47f0_0000_0000_0000)
}

fn double_wide_overflows(value: Quad) -> bool {
    // 2^1024 is one double-precision ULP above the largest finite value.
    value.abs() >= Quad::from_bits(0x43ff_u128 << 112)
}

/// Whether the exact sum of two values exceeds (or reaches) `limit` in
/// magnitude.
///
/// The wider APFloat formats can represent an exact single/double product,
/// but they still cannot retain a min-subnormal added to a maximum finite
/// value. Comparing the smaller magnitude with the exact distance to the
/// boundary avoids losing that information during the addition itself.
fn exact_sum_reaches<F: Float>(a: F, b: F, limit: F, inclusive: bool) -> bool {
    if a.is_nan() || b.is_nan() || a.is_infinite() || b.is_infinite() {
        return false;
    }

    let a_negative = a.is_negative() && !a.is_zero();
    let b_negative = b.is_negative() && !b.is_zero();
    let a = a.abs();
    let b = b.abs();

    if a_negative == b_negative {
        let (larger, smaller) = if a >= b { (a, b) } else { (b, a) };
        if larger > limit {
            return true;
        }
        if larger == limit {
            return inclusive || !smaller.is_zero();
        }
        let gap = limit.sub_r(larger, Round::NearestTiesToEven).value;
        smaller > gap || (inclusive && smaller == gap)
    } else {
        let (larger, smaller) = if a >= b { (a, b) } else { (b, a) };
        if larger < limit {
            return false;
        }
        if larger == limit {
            return inclusive && smaller.is_zero();
        }
        let excess = larger.sub_r(limit, Round::NearestTiesToEven).value;
        excess > smaller || (inclusive && excess == smaller)
    }
}

/// IEEE overflow is detected after rounding with an unbounded exponent. The
/// boundary is therefore rounding-mode dependent: just beyond max finite for
/// rounding toward the result's infinity, halfway to 2^emax for nearest, and
/// 2^emax for rounding inward.
fn rounded_sum_overflows<F: Float>(
    a: F,
    b: F,
    round: Round,
    max: F,
    midpoint: F,
    one_past: F,
) -> bool {
    let a_negative = a.is_negative() && !a.is_zero();
    let b_negative = b.is_negative() && !b.is_zero();
    let negative = if a_negative == b_negative || a.abs() >= b.abs() {
        a_negative
    } else {
        b_negative
    };

    let (limit, inclusive) = match round {
        Round::TowardPositive if !negative => (max, false),
        Round::TowardNegative if negative => (max, false),
        Round::NearestTiesToEven | Round::NearestTiesToAway => (midpoint, true),
        Round::TowardPositive | Round::TowardNegative | Round::TowardZero => (one_past, true),
    };
    exact_sum_reaches(a, b, limit, inclusive)
}

fn single_rounded_sum_overflows(a: Double, b: Double, round: Round) -> bool {
    rounded_sum_overflows(
        a,
        b,
        round,
        single_wide(Single::largest()),
        Double::from_bits(0x47ef_ffff_f000_0000),
        Double::from_bits(0x47f0_0000_0000_0000),
    )
}

fn double_rounded_sum_overflows(a: Quad, b: Quad, round: Round) -> bool {
    rounded_sum_overflows(
        a,
        b,
        round,
        double_wide(Double::largest()),
        Quad::from_bits((0x43fe_u128 << 112) | ((1_u128 << 112) - (1_u128 << 59))),
        Quad::from_bits(0x43ff_u128 << 112),
    )
}

fn sqrt_single(a: Single, round: Round) -> StatusAnd<Single> {
    if a.is_nan() {
        let status = if a.is_signaling() {
            Status::INVALID_OP
        } else {
            Status::OK
        };
        return status.and(Single::NAN);
    }
    if a.is_negative() && !a.is_zero() {
        return Status::INVALID_OP.and(Single::NAN);
    }
    if a.is_zero() || a.is_infinite() {
        return Status::OK.and(a);
    }

    let nearest = Single::from_bits(f32::from_bits(a.to_bits() as u32).sqrt().to_bits() as u128);
    sqrt_round_single(a, nearest, round)
}

fn sqrt_round_single(a: Single, nearest: Single, round: Round) -> StatusAnd<Single> {
    let mut loses_info = false;
    let a_quad: Quad = a.convert(&mut loses_info).value;
    let nearest_quad: Quad = nearest.convert(&mut loses_info).value;
    let square = nearest_quad.mul_r(nearest_quad, Round::NearestTiesToEven);
    let Some(ordering) = square.value.partial_cmp(&a_quad) else {
        return Status::INVALID_OP.and(Single::NAN);
    };
    if ordering == Ordering::Equal {
        return Status::OK.and(nearest);
    }

    let value = match (round, ordering) {
        (Round::TowardPositive, Ordering::Less) => nearest.next_up().value,
        (Round::TowardNegative | Round::TowardZero, Ordering::Greater) => nearest.next_down().value,
        // A representable binary input cannot have a square root exactly at
        // the midpoint between adjacent binary floats, so RMM equals RNE.
        _ => nearest,
    };
    Status::INEXACT.and(value)
}

fn sqrt_double(a: Double, round: Round) -> StatusAnd<Double> {
    if a.is_nan() {
        let status = if a.is_signaling() {
            Status::INVALID_OP
        } else {
            Status::OK
        };
        return status.and(Double::NAN);
    }
    if a.is_negative() && !a.is_zero() {
        return Status::INVALID_OP.and(Double::NAN);
    }
    if a.is_zero() || a.is_infinite() {
        return Status::OK.and(a);
    }

    let nearest = Double::from_bits(f64::from_bits(a.to_bits() as u64).sqrt().to_bits() as u128);
    sqrt_round_double(a, nearest, round)
}

fn sqrt_round_double(a: Double, nearest: Double, round: Round) -> StatusAnd<Double> {
    let mut loses_info = false;
    let a_quad: Quad = a.convert(&mut loses_info).value;
    let nearest_quad: Quad = nearest.convert(&mut loses_info).value;
    let square = nearest_quad.mul_r(nearest_quad, Round::NearestTiesToEven);
    let Some(ordering) = square.value.partial_cmp(&a_quad) else {
        return Status::INVALID_OP.and(Double::NAN);
    };
    if ordering == Ordering::Equal {
        return Status::OK.and(nearest);
    }

    let value = match (round, ordering) {
        (Round::TowardPositive, Ordering::Less) => nearest.next_up().value,
        (Round::TowardNegative | Round::TowardZero, Ordering::Greater) => nearest.next_down().value,
        _ => nearest,
    };
    Status::INEXACT.and(value)
}

/// Round to integral value per the given (validated) rounding mode.
fn round_rm(x: f64, rm: u8) -> f64 {
    match rm {
        0 => x.round_ties_even(), // RNE
        1 => x.trunc(),           // RTZ
        2 => x.floor(),           // RDN
        3 => x.ceil(),            // RUP
        4 => x.round(),           // RMM (ties away from zero)
        _ => unreachable!("rm validated by resolve_rm"),
    }
}

/// fp -> integer conversion with clamping. Returns (value, flags).
/// NaN -> max; out of range -> min/max; both set NV. In-range inexact -> NX.
fn fcvt_to_int(x: f64, rm: u8, min: i128, max: i128) -> (i128, u64) {
    if x.is_nan() {
        return (max, NV);
    }
    let r = round_rm(x, rm);
    // `as i128` on floats saturates, so infinities and huge finite values
    // compare correctly against the target range.
    let v = r as i128;
    if r.is_infinite() || v < min || v > max {
        return (if r < 0.0 { min } else { max }, NV);
    }
    let flags = if r != x { NX } else { 0 };
    (v, flags)
}

/// RISC-V fmin: both NaN -> canonical NaN; one NaN -> the other; -0.0 < +0.0.
fn min_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() && b.is_nan() {
        f32::from_bits(CANONICAL_NAN_F32)
    } else if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() {
            a
        } else {
            b
        }
    } else {
        a.min(b)
    }
}

fn max_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() && b.is_nan() {
        f32::from_bits(CANONICAL_NAN_F32)
    } else if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_positive() {
            a
        } else {
            b
        }
    } else {
        a.max(b)
    }
}

fn min_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() && b.is_nan() {
        f64::from_bits(CANONICAL_NAN_F64)
    } else if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() {
            a
        } else {
            b
        }
    } else {
        a.min(b)
    }
}

fn max_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() && b.is_nan() {
        f64::from_bits(CANONICAL_NAN_F64)
    } else if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_positive() {
            a
        } else {
            b
        }
    } else {
        a.max(b)
    }
}

/// fclass bitmask (identical bit layout for both formats).
fn classify(
    sign: bool,
    is_inf: bool,
    is_nan: bool,
    is_snan: bool,
    is_zero: bool,
    is_sub: bool,
) -> u64 {
    if is_nan {
        return if is_snan { 1 << 8 } else { 1 << 9 };
    }
    let bit = match (is_inf, is_zero, is_sub) {
        (true, _, _) => {
            if sign {
                0
            } else {
                7
            }
        }
        (_, true, _) => {
            if sign {
                3
            } else {
                4
            }
        }
        (_, _, true) => {
            if sign {
                2
            } else {
                5
            }
        }
        _ => {
            if sign {
                1
            } else {
                6
            }
        }
    };
    1u64 << bit
}

fn classify_f32(x: f32) -> u64 {
    classify(
        x.is_sign_negative(),
        x.is_infinite(),
        x.is_nan(),
        is_snan_f32(x),
        x == 0.0,
        x.is_subnormal(),
    )
}

fn classify_f64(x: f64) -> u64 {
    classify(
        x.is_sign_negative(),
        x.is_infinite(),
        x.is_nan(),
        is_snan_f64(x),
        x == 0.0,
        x.is_subnormal(),
    )
}

/// Execute one FP op (`Op::FpLoad`, `Op::FpStore`, or `Op::Fp`).
pub fn execute(cpu: &mut Cpu, bus: &mut Bus, raw: u32, op: &Op) -> Result<(), Exception> {
    if cpu.csr.fs() == csr::FS_OFF {
        return Err(Exception::IllegalInstruction(raw as u64));
    }
    match *op {
        Op::FpLoad { sz, frd, rs1, imm } => {
            let addr = cpu.reg(rs1).wrapping_add(imm as u64);
            match sz {
                FpSize::S => {
                    let v = cpu.load(bus, addr, 4)?;
                    cpu.fregs[frd] = nan_box(v as u32);
                }
                FpSize::D => {
                    cpu.fregs[frd] = cpu.load(bus, addr, 8)?;
                }
            }
            cpu.csr.set_fs_dirty();
            Ok(())
        }
        Op::FpStore { sz, rs1, frs2, imm } => {
            let addr = cpu.reg(rs1).wrapping_add(imm as u64);
            match sz {
                // fsw stores the raw low 32 bits (no unboxing/canonicalization).
                FpSize::S => cpu.store(bus, addr, cpu.fregs[frs2] & 0xFFFF_FFFF, 4),
                FpSize::D => cpu.store(bus, addr, cpu.fregs[frs2], 8),
            }
        }
        Op::Fp {
            op: fop,
            sz,
            rd,
            rs1,
            rs2,
            rs3,
            rm,
        } => exec_fp(cpu, raw, fop, sz, rd, rs1, rs2, rs3, rm),
        _ => unreachable!("non-FP op routed to fpu::execute"),
    }
}

#[allow(clippy::too_many_arguments)]
fn exec_fp(
    cpu: &mut Cpu,
    raw: u32,
    fop: FpOp,
    sz: FpSize,
    rd: usize,
    rs1: usize,
    rs2: usize,
    rs3: usize,
    rm: u8,
) -> Result<(), Exception> {
    use FpOp::*;
    match fop {
        Add | Sub | Mul | Div => {
            let round = ap_round(resolve_rm(cpu, raw, rm)?);
            match sz {
                FpSize::S => {
                    let a = Single::from_bits(unbox_bits(cpu.fregs[rs1]) as u128);
                    let b = Single::from_bits(unbox_bits(cpu.fregs[rs2]) as u128);
                    let result = match fop {
                        Add => a.add_r(b, round),
                        Sub => a.sub_r(b, round),
                        Mul => a.mul_r(b, round),
                        _ => a.div_r(b, round),
                    };
                    let overflowed = result.status.contains(Status::INEXACT) && {
                        let wide_a = single_wide(a);
                        let mut wide_b = single_wide(b);
                        match fop {
                            Add => single_rounded_sum_overflows(wide_a, wide_b, round),
                            Sub => {
                                wide_b = -wide_b;
                                single_rounded_sum_overflows(wide_a, wide_b, round)
                            }
                            Mul => single_wide_overflows(wide_a.mul_r(wide_b, round).value),
                            _ => single_wide_overflows(wide_a.div_r(wide_b, round).value),
                        }
                    };
                    let result = repair_tininess(result, || {
                        arithmetic(
                            fop,
                            extended::<_, ExtendedSingle>(a),
                            extended(b),
                            ExtendedSingle::ZERO,
                            round,
                        )
                        .value
                    });
                    write_ap_single(cpu, rd, result, overflowed);
                }
                FpSize::D => {
                    let a = Double::from_bits(cpu.fregs[rs1] as u128);
                    let b = Double::from_bits(cpu.fregs[rs2] as u128);
                    let result = match fop {
                        Add => a.add_r(b, round),
                        Sub => a.sub_r(b, round),
                        Mul => a.mul_r(b, round),
                        _ => a.div_r(b, round),
                    };
                    let overflowed = result.status.contains(Status::INEXACT) && {
                        let wide_a = double_wide(a);
                        let mut wide_b = double_wide(b);
                        match fop {
                            Add => double_rounded_sum_overflows(wide_a, wide_b, round),
                            Sub => {
                                wide_b = -wide_b;
                                double_rounded_sum_overflows(wide_a, wide_b, round)
                            }
                            Mul => double_wide_overflows(wide_a.mul_r(wide_b, round).value),
                            _ => double_wide_overflows(wide_a.div_r(wide_b, round).value),
                        }
                    };
                    let result = repair_tininess(result, || {
                        arithmetic(
                            fop,
                            extended::<_, ExtendedDouble>(a),
                            extended(b),
                            ExtendedDouble::ZERO,
                            round,
                        )
                        .value
                    });
                    write_ap_double(cpu, rd, result, overflowed);
                }
            }
        }
        Sqrt => {
            let round = ap_round(resolve_rm(cpu, raw, rm)?);
            match sz {
                FpSize::S => {
                    let a = Single::from_bits(unbox_bits(cpu.fregs[rs1]) as u128);
                    write_ap_single(cpu, rd, sqrt_single(a, round), false);
                }
                FpSize::D => {
                    let a = Double::from_bits(cpu.fregs[rs1] as u128);
                    write_ap_double(cpu, rd, sqrt_double(a, round), false);
                }
            }
        }
        Madd | Msub | Nmsub | Nmadd => {
            let round = ap_round(resolve_rm(cpu, raw, rm)?);
            match sz {
                FpSize::S => {
                    let a = Single::from_bits(unbox_bits(cpu.fregs[rs1]) as u128);
                    let b = Single::from_bits(unbox_bits(cpu.fregs[rs2]) as u128);
                    let c = Single::from_bits(unbox_bits(cpu.fregs[rs3]) as u128);
                    let result = match fop {
                        Madd => a.mul_add_r(b, c, round),
                        Msub => a.mul_add_r(b, -c, round),
                        Nmsub => (-a).mul_add_r(b, c, round),
                        _ => (-a).mul_add_r(b, -c, round),
                    };
                    let overflowed = result.status.contains(Status::INEXACT) && {
                        let wide_a = single_wide(a);
                        let wide_b = single_wide(b);
                        let mut wide_c = single_wide(c);
                        let mut product = wide_a.mul_r(wide_b, Round::NearestTiesToEven).value;
                        match fop {
                            Madd => {}
                            Msub => wide_c = -wide_c,
                            Nmsub => product = -product,
                            _ => {
                                product = -product;
                                wide_c = -wide_c;
                            }
                        }
                        single_rounded_sum_overflows(product, wide_c, round)
                    };
                    let result = repair_tininess(result, || {
                        arithmetic(
                            fop,
                            extended::<_, ExtendedSingle>(a),
                            extended(b),
                            extended(c),
                            round,
                        )
                        .value
                    });
                    write_ap_single(cpu, rd, result, overflowed);
                }
                FpSize::D => {
                    let a = Double::from_bits(cpu.fregs[rs1] as u128);
                    let b = Double::from_bits(cpu.fregs[rs2] as u128);
                    let c = Double::from_bits(cpu.fregs[rs3] as u128);
                    let result = match fop {
                        Madd => a.mul_add_r(b, c, round),
                        Msub => a.mul_add_r(b, -c, round),
                        Nmsub => (-a).mul_add_r(b, c, round),
                        _ => (-a).mul_add_r(b, -c, round),
                    };
                    let overflowed = result.status.contains(Status::INEXACT) && {
                        let wide_a = double_wide(a);
                        let wide_b = double_wide(b);
                        let mut wide_c = double_wide(c);
                        let mut product = wide_a.mul_r(wide_b, Round::NearestTiesToEven).value;
                        match fop {
                            Madd => {}
                            Msub => wide_c = -wide_c,
                            Nmsub => product = -product,
                            _ => {
                                product = -product;
                                wide_c = -wide_c;
                            }
                        }
                        double_rounded_sum_overflows(product, wide_c, round)
                    };
                    let result = repair_tininess(result, || {
                        arithmetic(
                            fop,
                            extended::<_, ExtendedDouble>(a),
                            extended(b),
                            extended(c),
                            round,
                        )
                        .value
                    });
                    write_ap_double(cpu, rd, result, overflowed);
                }
            }
        }
        Sgnj | Sgnjn | Sgnjx => match sz {
            FpSize::S => {
                let a = unbox_bits(cpu.fregs[rs1]);
                let b = unbox_bits(cpu.fregs[rs2]);
                const SIGN: u32 = 0x8000_0000;
                let bits = match fop {
                    Sgnj => (a & !SIGN) | (b & SIGN),
                    Sgnjn => (a & !SIGN) | (!b & SIGN),
                    _ => a ^ (b & SIGN),
                };
                write_f32_raw(cpu, rd, bits);
            }
            FpSize::D => {
                let a = cpu.fregs[rs1];
                let b = cpu.fregs[rs2];
                const SIGN: u64 = 1 << 63;
                let bits = match fop {
                    Sgnj => (a & !SIGN) | (b & SIGN),
                    Sgnjn => (a & !SIGN) | (!b & SIGN),
                    _ => a ^ (b & SIGN),
                };
                write_f64_raw(cpu, rd, bits);
            }
        },
        Min | Max => match sz {
            FpSize::S => {
                let a = unbox_f32(cpu.fregs[rs1]);
                let b = unbox_f32(cpu.fregs[rs2]);
                if is_snan_f32(a) || is_snan_f32(b) {
                    set_fflags(cpu, NV);
                }
                let r = if fop == Min {
                    min_f32(a, b)
                } else {
                    max_f32(a, b)
                };
                write_f32(cpu, rd, r);
            }
            FpSize::D => {
                let a = f64::from_bits(cpu.fregs[rs1]);
                let b = f64::from_bits(cpu.fregs[rs2]);
                if is_snan_f64(a) || is_snan_f64(b) {
                    set_fflags(cpu, NV);
                }
                let r = if fop == Min {
                    min_f64(a, b)
                } else {
                    max_f64(a, b)
                };
                write_f64(cpu, rd, r);
            }
        },
        Eq | Lt | Le => {
            // Integer destination: FS only dirtied if fflags change.
            let (res, fl) = match sz {
                FpSize::S => {
                    let a = unbox_f32(cpu.fregs[rs1]);
                    let b = unbox_f32(cpu.fregs[rs2]);
                    match fop {
                        // FEQ is quiet: NV only on signaling NaN.
                        Eq => (
                            a == b,
                            if is_snan_f32(a) || is_snan_f32(b) {
                                NV
                            } else {
                                0
                            },
                        ),
                        // FLT/FLE are signaling: NV on any NaN (result 0).
                        Lt => (a < b, if a.is_nan() || b.is_nan() { NV } else { 0 }),
                        _ => (a <= b, if a.is_nan() || b.is_nan() { NV } else { 0 }),
                    }
                }
                FpSize::D => {
                    let a = f64::from_bits(cpu.fregs[rs1]);
                    let b = f64::from_bits(cpu.fregs[rs2]);
                    match fop {
                        Eq => (
                            a == b,
                            if is_snan_f64(a) || is_snan_f64(b) {
                                NV
                            } else {
                                0
                            },
                        ),
                        Lt => (a < b, if a.is_nan() || b.is_nan() { NV } else { 0 }),
                        _ => (a <= b, if a.is_nan() || b.is_nan() { NV } else { 0 }),
                    }
                }
            };
            set_fflags(cpu, fl);
            cpu.set_reg(rd, res as u64);
        }
        Class => {
            let mask = match sz {
                FpSize::S => classify_f32(unbox_f32(cpu.fregs[rs1])),
                FpSize::D => classify_f64(f64::from_bits(cpu.fregs[rs1])),
            };
            cpu.set_reg(rd, mask);
        }
        CvtWFmt | CvtWuFmt | CvtLFmt | CvtLuFmt => {
            let eff_rm = resolve_rm(cpu, raw, rm)?;
            // Widening f32 -> f64 is exact, so a single f64 path is safe.
            let x = match sz {
                FpSize::S => unbox_f32(cpu.fregs[rs1]) as f64,
                FpSize::D => f64::from_bits(cpu.fregs[rs1]),
            };
            let (min, max) = match fop {
                CvtWFmt => (i32::MIN as i128, i32::MAX as i128),
                CvtWuFmt => (0, u32::MAX as i128),
                CvtLFmt => (i64::MIN as i128, i64::MAX as i128),
                _ => (0, u64::MAX as i128),
            };
            let (v, fl) = fcvt_to_int(x, eff_rm, min, max);
            let out = match fop {
                // W forms sign-extend the 32-bit result (WU included, per spec).
                CvtWFmt => (v as i32) as i64 as u64,
                CvtWuFmt => (v as u32) as i32 as i64 as u64,
                CvtLFmt => (v as i64) as u64,
                _ => v as u64,
            };
            set_fflags(cpu, fl);
            cpu.set_reg(rd, out);
        }
        CvtFmtW | CvtFmtWu | CvtFmtL | CvtFmtLu => {
            let round = ap_round(resolve_rm(cpu, raw, rm)?);
            match sz {
                FpSize::S => {
                    let result = match fop {
                        CvtFmtW => Single::from_i128_r((cpu.reg(rs1) as i32) as i128, round),
                        CvtFmtWu => Single::from_u128_r((cpu.reg(rs1) as u32) as u128, round),
                        CvtFmtL => Single::from_i128_r((cpu.reg(rs1) as i64) as i128, round),
                        _ => Single::from_u128_r(cpu.reg(rs1) as u128, round),
                    };
                    write_ap_single(cpu, rd, result, false);
                }
                FpSize::D => {
                    let result = match fop {
                        CvtFmtW => Double::from_i128_r((cpu.reg(rs1) as i32) as i128, round),
                        CvtFmtWu => Double::from_u128_r((cpu.reg(rs1) as u32) as u128, round),
                        CvtFmtL => Double::from_i128_r((cpu.reg(rs1) as i64) as i128, round),
                        _ => Double::from_u128_r(cpu.reg(rs1) as u128, round),
                    };
                    write_ap_double(cpu, rd, result, false);
                }
            }
        }
        CvtFmtFmt => {
            let round = ap_round(resolve_rm(cpu, raw, rm)?);
            match sz {
                // fcvt.s.d: narrow f64 -> f32.
                FpSize::S => {
                    let a = Double::from_bits(cpu.fregs[rs1] as u128);
                    let mut loses_info = false;
                    let result: StatusAnd<Single> = a.convert_r(round, &mut loses_info);
                    let result = repair_tininess(result, || {
                        let value: StatusAnd<ExtendedSingle> = a.convert_r(round, &mut false);
                        value.value
                    });
                    write_ap_single(cpu, rd, result, single_wide_overflows(a));
                }
                // fcvt.d.s: widen f32 -> f64 (exact).
                FpSize::D => {
                    let a = Single::from_bits(unbox_bits(cpu.fregs[rs1]) as u128);
                    let mut loses_info = false;
                    let result: StatusAnd<Double> = a.convert_r(round, &mut loses_info);
                    write_ap_double(cpu, rd, result, false);
                }
            }
        }
        MvXFmt => {
            // Raw bit move to the integer register; no fflags, no FS dirty.
            let v = match sz {
                FpSize::S => (cpu.fregs[rs1] as u32 as i32) as i64 as u64,
                FpSize::D => cpu.fregs[rs1],
            };
            cpu.set_reg(rd, v);
        }
        MvFmtX => match sz {
            FpSize::S => write_f32_raw(cpu, rd, cpu.reg(rs1) as u32),
            FpSize::D => write_f64_raw(cpu, rd, cpu.reg(rs1)),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::csr::{FS_CLEAN, FS_DIRTY};

    // Register aliases to keep tests readable.
    const F0: usize = 0;
    const F1: usize = 1;
    const F2: usize = 2;
    const F3: usize = 3;
    const X5: usize = 5;
    const X6: usize = 6;

    fn setup() -> (Cpu, Bus) {
        let mut cpu = Cpu::new();
        cpu.csr.set_fs_dirty(); // enable FP
        (cpu, Bus::new(4096))
    }

    fn fp_op(op: FpOp, sz: FpSize, rd: usize, rs1: usize, rs2: usize, rm: u8) -> Op {
        Op::Fp {
            op,
            sz,
            rd,
            rs1,
            rs2,
            rs3: 0,
            rm,
        }
    }

    fn fused(op: FpOp, sz: FpSize, rd: usize, rs1: usize, rs2: usize, rs3: usize) -> Op {
        Op::Fp {
            op,
            sz,
            rd,
            rs1,
            rs2,
            rs3,
            rm: 0,
        }
    }

    fn run(cpu: &mut Cpu, bus: &mut Bus, op: Op) {
        execute(cpu, bus, 0, &op).expect("fp op should not trap");
    }

    fn fflags(cpu: &Cpu) -> u64 {
        cpu.csr.load_raw(csr::FFLAGS)
    }

    fn clear_fflags(cpu: &mut Cpu) {
        let f = cpu.csr.load_raw(csr::FCSR);
        cpu.csr.store_raw(csr::FCSR, f & !0x1F);
    }

    fn set_f32(cpu: &mut Cpu, r: usize, v: f32) {
        cpu.fregs[r] = nan_box(v.to_bits());
    }

    fn set_f64(cpu: &mut Cpu, r: usize, v: f64) {
        cpu.fregs[r] = v.to_bits();
    }

    #[test]
    fn fs_off_raises_illegal() {
        // Fresh CPU: mstatus reset value has FS == Off.
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(4096);
        assert_eq!(cpu.csr.fs(), csr::FS_OFF);
        let op = fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 0);
        assert_eq!(
            execute(&mut cpu, &mut bus, 0x1234, &op),
            Err(Exception::IllegalInstruction(0x1234))
        );
        // Even a pure move is illegal with FS off.
        let mv = fp_op(FpOp::MvFmtX, FpSize::S, F0, X5, 0, 0);
        assert!(matches!(
            execute(&mut cpu, &mut bus, 0, &mv),
            Err(Exception::IllegalInstruction(_))
        ));
    }

    #[test]
    fn nan_boxing_round_trip() {
        let (mut cpu, mut bus) = setup();
        // fmv.w.x boxes the low 32 bits.
        cpu.set_reg(X5, 0x3F80_0000); // 1.0f32
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::MvFmtX, FpSize::S, F1, X5, 0, 0),
        );
        assert_eq!(cpu.fregs[F1], 0xFFFF_FFFF_3F80_0000);
        // fmv.x.w reads back the raw low bits, sign-extended.
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::MvXFmt, FpSize::S, X6, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X6), 0x3F80_0000);
        // A negative bit pattern sign-extends.
        cpu.set_reg(X5, 0xBF80_0000); // -1.0f32
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::MvFmtX, FpSize::S, F1, X5, 0, 0),
        );
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::MvXFmt, FpSize::S, X6, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X6), 0xFFFF_FFFF_BF80_0000);
    }

    #[test]
    fn improperly_boxed_operand_is_canonical_nan() {
        let (mut cpu, mut bus) = setup();
        cpu.fregs[F1] = 0x0000_0000_3F80_0000; // 1.0f32 bits but NOT boxed
        set_f32(&mut cpu, F2, 2.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 0),
        );
        // Operand read as canonical NaN -> canonical NaN result.
        assert_eq!(cpu.fregs[F0], nan_box(CANONICAL_NAN_F32));
    }

    #[test]
    fn basic_arithmetic() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, 1.5);
        set_f32(&mut cpu, F2, 2.25);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(cpu.fregs[F0], nan_box(3.75f32.to_bits()));

        set_f64(&mut cpu, F1, 1.5);
        set_f64(&mut cpu, F2, 0.25);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Mul, FpSize::D, F0, F1, F2, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 0.375);

        set_f64(&mut cpu, F1, 2.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sqrt, FpSize::D, F0, F1, 0, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 2.0f64.sqrt());
    }

    #[test]
    fn arithmetic_honors_rounding_mode_and_sets_inexact() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, 1.0);
        set_f32(&mut cpu, F2, f32::EPSILON / 2.0);

        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), 1.0f32.to_bits());
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 3),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), 1.0f32.to_bits() + 1);
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 4),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), 1.0f32.to_bits() + 1);
        assert_eq!(fflags(&cpu), NX);
    }

    #[test]
    fn sqrt_sets_inexact_and_honors_directed_rounding() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, 2.0);

        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sqrt, FpSize::S, F0, F1, 0, 0),
        );
        let nearest = 2.0f32.sqrt().to_bits();
        assert_eq!(unbox_bits(cpu.fregs[F0]), nearest);
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sqrt, FpSize::S, F0, F1, 0, 3),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), nearest + 1);
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        set_f32(&mut cpu, F1, 4.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sqrt, FpSize::S, F0, F1, 0, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), 2.0);
        assert_eq!(fflags(&cpu), 0);
    }

    #[test]
    fn invalid_ops_produce_canonical_nan_and_nv() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, f32::INFINITY);
        set_f32(&mut cpu, F2, f32::INFINITY);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sub, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(cpu.fregs[F0], nan_box(CANONICAL_NAN_F32));
        assert_ne!(fflags(&cpu) & NV, 0, "inf - inf sets NV");

        clear_fflags(&mut cpu);
        set_f64(&mut cpu, F1, 0.0);
        set_f64(&mut cpu, F2, 0.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Div, FpSize::D, F0, F1, F2, 0),
        );
        assert_eq!(cpu.fregs[F0], CANONICAL_NAN_F64);
        assert_ne!(fflags(&cpu) & NV, 0, "0/0 sets NV");

        clear_fflags(&mut cpu);
        set_f64(&mut cpu, F1, -1.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sqrt, FpSize::D, F0, F1, 0, 0),
        );
        assert_eq!(cpu.fregs[F0], CANONICAL_NAN_F64);
        assert_ne!(fflags(&cpu) & NV, 0, "sqrt(-1) sets NV");
    }

    #[test]
    fn divide_by_zero_sets_dz() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, 3.0);
        set_f32(&mut cpu, F2, 0.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Div, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), f32::INFINITY);
        assert_eq!(fflags(&cpu), DZ);
    }

    #[test]
    fn min_max_zero_and_nan_rules() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, -0.0);
        set_f32(&mut cpu, F2, 0.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Min, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(
            unbox_bits(cpu.fregs[F0]),
            (-0.0f32).to_bits(),
            "min(-0,+0) = -0"
        );
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Max, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(
            unbox_bits(cpu.fregs[F0]),
            0.0f32.to_bits(),
            "max(-0,+0) = +0"
        );

        // One NaN -> the other operand; qNaN sets no NV.
        set_f64(&mut cpu, F1, f64::NAN);
        set_f64(&mut cpu, F2, 5.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Min, FpSize::D, F0, F1, F2, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 5.0);
        assert_eq!(fflags(&cpu) & NV, 0, "quiet NaN in min sets no NV");

        // Both NaN -> canonical NaN.
        set_f64(&mut cpu, F2, f64::NAN);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Max, FpSize::D, F0, F1, F2, 0),
        );
        assert_eq!(cpu.fregs[F0], CANONICAL_NAN_F64);

        // sNaN input sets NV.
        cpu.fregs[F1] = nan_box(0x7F80_0001); // f32 sNaN
        set_f32(&mut cpu, F2, 1.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Min, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), 1.0);
        assert_ne!(fflags(&cpu) & NV, 0, "sNaN in min sets NV");
    }

    #[test]
    fn compares_quiet_vs_signaling() {
        let (mut cpu, mut bus) = setup();
        set_f64(&mut cpu, F1, 1.0);
        set_f64(&mut cpu, F2, 2.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Lt, FpSize::D, X5, F1, F2, 0),
        );
        assert_eq!(cpu.reg(X5), 1);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Le, FpSize::D, X5, F2, F1, 0),
        );
        assert_eq!(cpu.reg(X5), 0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Eq, FpSize::D, X5, F1, F1, 0),
        );
        assert_eq!(cpu.reg(X5), 1);
        assert_eq!(fflags(&cpu), 0);

        // FEQ with quiet NaN: 0, no NV.
        set_f64(&mut cpu, F2, f64::NAN);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Eq, FpSize::D, X5, F1, F2, 0),
        );
        assert_eq!(cpu.reg(X5), 0);
        assert_eq!(fflags(&cpu) & NV, 0, "feq with qNaN is quiet");

        // FLT with quiet NaN: 0, NV set.
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Lt, FpSize::D, X5, F1, F2, 0),
        );
        assert_eq!(cpu.reg(X5), 0);
        assert_ne!(fflags(&cpu) & NV, 0, "flt with NaN signals NV");

        // FEQ with signaling NaN: NV set.
        clear_fflags(&mut cpu);
        cpu.fregs[F2] = 0x7FF0_0000_0000_0001; // f64 sNaN
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Eq, FpSize::D, X5, F1, F2, 0),
        );
        assert_eq!(cpu.reg(X5), 0);
        assert_ne!(fflags(&cpu) & NV, 0, "feq with sNaN signals NV");
    }

    #[test]
    fn integer_dest_does_not_dirty_fs_unless_fflags() {
        let (mut cpu, mut bus) = setup();
        set_f64(&mut cpu, F1, 1.0);
        set_f64(&mut cpu, F2, 2.0);
        // Force FS = Clean, then run a compare that raises no flags.
        cpu.csr.store_raw(csr::MSTATUS, FS_CLEAN << 13);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Lt, FpSize::D, X5, F1, F2, 0),
        );
        assert_eq!(cpu.csr.fs(), FS_CLEAN, "flag-free compare keeps FS clean");
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Class, FpSize::D, X5, F1, 0, 0),
        );
        assert_eq!(cpu.csr.fs(), FS_CLEAN, "fclass keeps FS clean");
        // A compare that sets NV dirties FS.
        set_f64(&mut cpu, F2, f64::NAN);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Lt, FpSize::D, X5, F1, F2, 0),
        );
        assert_eq!(cpu.csr.fs(), FS_DIRTY, "fflags update dirties FS");
    }

    #[test]
    fn fclass_bits() {
        let (mut cpu, mut bus) = setup();
        let cases: [(f64, u64); 8] = [
            (f64::NEG_INFINITY, 1 << 0),
            (-1.5, 1 << 1),
            (-f64::MIN_POSITIVE / 2.0, 1 << 2), // negative subnormal
            (-0.0, 1 << 3),
            (0.0, 1 << 4),
            (f64::MIN_POSITIVE / 2.0, 1 << 5), // positive subnormal
            (42.0, 1 << 6),
            (f64::INFINITY, 1 << 7),
        ];
        for (v, expect) in cases {
            set_f64(&mut cpu, F1, v);
            run(
                &mut cpu,
                &mut bus,
                fp_op(FpOp::Class, FpSize::D, X5, F1, 0, 0),
            );
            assert_eq!(cpu.reg(X5), expect, "fclass.d of {v}");
        }
        // sNaN and qNaN.
        cpu.fregs[F1] = 0x7FF0_0000_0000_0001;
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Class, FpSize::D, X5, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X5), 1 << 8, "fclass.d sNaN");
        set_f64(&mut cpu, F1, f64::NAN);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Class, FpSize::D, X5, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X5), 1 << 9, "fclass.d qNaN");
        // Single-precision path, with an unboxed value classifying as qNaN.
        set_f32(&mut cpu, F1, -1.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Class, FpSize::S, X5, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X5), 1 << 1, "fclass.s -1.0");
        cpu.fregs[F1] = 0x0000_0000_3F80_0000; // not boxed
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Class, FpSize::S, X5, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X5), 1 << 9, "unboxed f32 classifies as qNaN");
    }

    #[test]
    fn fcvt_to_int_clamping() {
        let (mut cpu, mut bus) = setup();
        // fcvt.w.s of 3e9 -> i32::MAX, NV.
        set_f32(&mut cpu, F1, 3e9);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWFmt, FpSize::S, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), i32::MAX as i64 as u64);
        assert_ne!(fflags(&cpu) & NV, 0);

        // fcvt.w.s of -3e9 -> i32::MIN (sign-extended), NV.
        clear_fflags(&mut cpu);
        set_f32(&mut cpu, F1, -3e9);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWFmt, FpSize::S, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), i32::MIN as i64 as u64);
        assert_ne!(fflags(&cpu) & NV, 0);

        // NaN -> max, NV.
        clear_fflags(&mut cpu);
        set_f32(&mut cpu, F1, f32::NAN);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWFmt, FpSize::S, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), i32::MAX as i64 as u64);
        assert_ne!(fflags(&cpu) & NV, 0);

        // Negative to unsigned -> 0, NV.
        clear_fflags(&mut cpu);
        set_f32(&mut cpu, F1, -1.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWuFmt, FpSize::S, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), 0);
        assert_ne!(fflags(&cpu) & NV, 0);

        // fcvt.wu.s of u32::MAX-ish in-range value sign-extends the pattern.
        clear_fflags(&mut cpu);
        set_f32(&mut cpu, F1, 4.0e9); // rounds to 4e9 < 2^32
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWuFmt, FpSize::S, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), (4_000_000_000u32 as i32) as i64 as u64);

        // fcvt.l.d in range, exact.
        set_f64(&mut cpu, F1, -123456789.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtLFmt, FpSize::D, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5) as i64, -123456789);

        // fcvt.lu.d of a value above u64 range -> u64::MAX, NV.
        clear_fflags(&mut cpu);
        set_f64(&mut cpu, F1, 2.5e19);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtLuFmt, FpSize::D, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), u64::MAX);
        assert_ne!(fflags(&cpu) & NV, 0);
    }

    #[test]
    fn fcvt_rounding_modes() {
        let (mut cpu, mut bus) = setup();
        set_f64(&mut cpu, F1, -1.5);
        let modes: [(u8, i64); 5] = [
            (0, -2), // RNE: -1.5 ties to even -> -2
            (1, -1), // RTZ
            (2, -2), // RDN (floor)
            (3, -1), // RUP (ceil)
            (4, -2), // RMM (half away from zero)
        ];
        for (rm, expect) in modes {
            run(
                &mut cpu,
                &mut bus,
                fp_op(FpOp::CvtWFmt, FpSize::D, X5, F1, 0, rm),
            );
            assert_eq!(cpu.reg(X5) as i64, expect, "fcvt.w.d(-1.5) rm={rm}");
        }
        // RNE ties-to-even at +2.5 -> 2, RTZ 2.5 -> 2 with NX.
        set_f64(&mut cpu, F1, 2.5);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWFmt, FpSize::D, X5, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X5), 2, "RNE 2.5 -> 2 (ties to even)");
        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWFmt, FpSize::D, X5, F1, 0, 1),
        );
        assert_eq!(cpu.reg(X5), 2);
        assert_ne!(fflags(&cpu) & super::NX, 0, "inexact conversion sets NX");
        // Exact conversion sets no flags.
        clear_fflags(&mut cpu);
        set_f64(&mut cpu, F1, -4.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtWFmt, FpSize::D, X5, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X5) as i64, -4);
        assert_eq!(fflags(&cpu), 0);
    }

    #[test]
    fn invalid_rounding_modes_raise_illegal() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, 1.0);
        set_f32(&mut cpu, F2, 1.0);
        for rm in [5u8, 6] {
            let op = fp_op(FpOp::Add, FpSize::S, F0, F1, F2, rm);
            assert_eq!(
                execute(&mut cpu, &mut bus, 0xABCD, &op),
                Err(Exception::IllegalInstruction(0xABCD)),
                "rm={rm} must be illegal"
            );
        }
        // Dynamic rm with invalid frm.
        cpu.csr.store_raw(csr::FCSR, 5 << 5); // frm = 5
        let op = fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 7);
        assert!(matches!(
            execute(&mut cpu, &mut bus, 0, &op),
            Err(Exception::IllegalInstruction(_))
        ));
        // Dynamic rm with valid frm works.
        cpu.csr.store_raw(csr::FCSR, 0); // frm = 0 (RNE)
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 7),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), 2.0);
    }

    #[test]
    fn int_to_fp_conversions() {
        let (mut cpu, mut bus) = setup();
        cpu.set_reg(X5, 5);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtW, FpSize::S, F0, X5, 0, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), 5.0);

        // Negative i32 in a 64-bit register (upper bits set).
        cpu.set_reg(X5, (-7i32) as u32 as u64);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtW, FpSize::D, F0, X5, 0, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), -7.0);

        // Same bits as unsigned.
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtWu, FpSize::D, F0, X5, 0, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), ((-7i32) as u32) as f64);

        // u64::MAX -> f64 is inexact (NX).
        clear_fflags(&mut cpu);
        cpu.set_reg(X5, u64::MAX);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtLu, FpSize::D, F0, X5, 0, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), u64::MAX as f64);
        assert_ne!(fflags(&cpu) & super::NX, 0, "u64::MAX -> f64 is inexact");

        // Exact i64 -> f64: no flags.
        clear_fflags(&mut cpu);
        cpu.set_reg(X5, 1024u64);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtL, FpSize::D, F0, X5, 0, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 1024.0);
        assert_eq!(fflags(&cpu), 0);
    }

    #[test]
    fn fmt_to_fmt_conversion() {
        let (mut cpu, mut bus) = setup();
        // fcvt.d.s widen (exact).
        set_f32(&mut cpu, F1, 1.5);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtFmt, FpSize::D, F0, F1, 0, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 1.5);

        // fcvt.s.d overflow -> inf, OF|NX.
        clear_fflags(&mut cpu);
        set_f64(&mut cpu, F1, 1e300);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtFmt, FpSize::S, F0, F1, 0, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), f32::INFINITY);
        assert_eq!(fflags(&cpu) & (OF | super::NX), OF | super::NX);

        // NaN narrows to the canonical f32 NaN.
        set_f64(&mut cpu, F1, f64::NAN);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtFmt, FpSize::S, F0, F1, 0, 0),
        );
        assert_eq!(cpu.fregs[F0], nan_box(CANONICAL_NAN_F32));
    }

    #[test]
    fn fcvt_s_d_above_max_without_exponent_overflow_is_only_inexact() {
        let (mut cpu, mut bus) = setup();
        let max = f64::from(f32::MAX);
        let just_above_max = f64::from_bits(max.to_bits() + 1);

        for rm in [0, 1] {
            clear_fflags(&mut cpu);
            set_f64(&mut cpu, F1, just_above_max);
            run(
                &mut cpu,
                &mut bus,
                fp_op(FpOp::CvtFmtFmt, FpSize::S, F0, F1, 0, rm),
            );

            assert_eq!(unbox_f32(cpu.fregs[F0]), f32::MAX);
            assert_eq!(fflags(&cpu), NX, "rm={rm}");
        }
    }

    #[test]
    fn fcvt_s_d_tininess_is_detected_after_directed_rounding() {
        let (mut cpu, mut bus) = setup();
        let min_normal = f64::from(f32::MIN_POSITIVE);
        let max_subnormal = f64::from(f32::from_bits(0x007F_FFFF));
        let midpoint = (min_normal + max_subnormal) / 2.0;
        let just_below_midpoint = f64::from_bits(midpoint.to_bits() - 1);

        // Below the midpoint, unbounded-exponent rounding is still tiny,
        // even though the stored result rounds up to minimum normal.
        set_f64(&mut cpu, F1, just_below_midpoint);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtFmt, FpSize::S, F0, F1, 0, 3),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), 0x0080_0000);
        assert_eq!(fflags(&cpu), UF | NX);

        // RTZ produces a subnormal result from the same exact input.
        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::CvtFmtFmt, FpSize::S, F0, F1, 0, 1),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), 0x007F_FFFF);
        assert_eq!(fflags(&cpu), UF | NX);
    }

    #[test]
    fn fcvt_s_d_normal_result_can_still_underflow() {
        let (mut cpu, mut bus) = setup();
        // SoftFloat FAQ's after-rounding tininess example.
        for (bits, flags) in [
            (0x380F_FFFF_E100_0000, UF | NX),
            (0x380F_FFFF_F000_0000, NX),
        ] {
            for sign in [0, 1_u64 << 63] {
                clear_fflags(&mut cpu);
                cpu.fregs[F1] = bits | sign;
                run(
                    &mut cpu,
                    &mut bus,
                    fp_op(FpOp::CvtFmtFmt, FpSize::S, F0, F1, 0, 0),
                );
                assert_eq!(
                    unbox_bits(cpu.fregs[F0]),
                    0x0080_0000 | ((sign >> 32) as u32)
                );
                assert_eq!(fflags(&cpu), flags, "input={bits:x} sign={sign:x}");
            }
        }
    }

    #[test]
    fn sgnj_bit_tricks() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, 1.5);
        set_f32(&mut cpu, F2, -2.0);
        // fsgnj.s: magnitude of rs1, sign of rs2.
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sgnj, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), -1.5);
        // fneg.s == fsgnjn.s f0, f1, f1.
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sgnjn, FpSize::S, F0, F1, F1, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), -1.5);
        // fabs.s == fsgnjx.s f0, f2, f2.
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sgnjx, FpSize::S, F0, F2, F2, 0),
        );
        assert_eq!(unbox_f32(cpu.fregs[F0]), 2.0);
        // sgnj preserves NaN payloads (raw bit op, no canonicalization).
        cpu.fregs[F1] = nan_box(0xFFC0_0001);
        set_f32(&mut cpu, F2, 1.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sgnj, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(unbox_bits(cpu.fregs[F0]), 0x7FC0_0001);
        // Double precision fsgnjx flips via XOR.
        set_f64(&mut cpu, F1, -3.0);
        set_f64(&mut cpu, F2, -1.0);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Sgnjx, FpSize::D, F0, F1, F2, 0),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 3.0);
    }

    #[test]
    fn fused_multiply_add_single_rounding() {
        let (mut cpu, mut bus) = setup();
        // a*a is not exactly representable in f32; the fused form keeps the
        // low bits that a separate multiply would round away.
        let a = 4097.0f32; // 2^12 + 1; a*a = 2^24 + 2^13 + 1
        let c = -(a * a); // rounded product, negated
        set_f32(&mut cpu, F1, a);
        set_f32(&mut cpu, F2, a);
        set_f32(&mut cpu, F3, c);
        run(
            &mut cpu,
            &mut bus,
            fused(FpOp::Madd, FpSize::S, F0, F1, F2, F3),
        );
        let fused_res = unbox_f32(cpu.fregs[F0]);
        assert_eq!(fused_res, a.mul_add(a, c), "matches host fma");
        assert_ne!(fused_res, a * a + c, "fused differs from double rounding");

        // Sign conventions.
        set_f64(&mut cpu, F1, 2.0);
        set_f64(&mut cpu, F2, 3.0);
        set_f64(&mut cpu, F3, 1.0);
        run(
            &mut cpu,
            &mut bus,
            fused(FpOp::Madd, FpSize::D, F0, F1, F2, F3),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 7.0, "madd = a*b + c");
        run(
            &mut cpu,
            &mut bus,
            fused(FpOp::Msub, FpSize::D, F0, F1, F2, F3),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), 5.0, "msub = a*b - c");
        run(
            &mut cpu,
            &mut bus,
            fused(FpOp::Nmsub, FpSize::D, F0, F1, F2, F3),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), -5.0, "nmsub = -(a*b) + c");
        run(
            &mut cpu,
            &mut bus,
            fused(FpOp::Nmadd, FpSize::D, F0, F1, F2, F3),
        );
        assert_eq!(f64::from_bits(cpu.fregs[F0]), -7.0, "nmadd = -(a*b) - c");
    }

    #[test]
    fn directed_finite_overflow_sets_overflow_and_inexact() {
        let (mut cpu, mut bus) = setup();
        set_f32(&mut cpu, F1, f32::MAX);
        set_f32(&mut cpu, F2, f32::MAX);

        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 1),
        );

        assert_eq!(fflags(&cpu), OF | NX);
    }

    #[test]
    fn tiny_addend_past_maximum_sets_overflow() {
        let (mut cpu, mut bus) = setup();

        set_f32(&mut cpu, F1, f32::MAX);
        cpu.fregs[F2] = nan_box(1);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 0),
        );
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::S, F0, F1, F2, 3),
        );
        assert_eq!(fflags(&cpu), OF | NX);

        clear_fflags(&mut cpu);
        set_f64(&mut cpu, F1, f64::MAX);
        cpu.fregs[F2] = 1;
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::D, F0, F1, F2, 0),
        );
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::Add, FpSize::D, F0, F1, F2, 3),
        );
        assert_eq!(fflags(&cpu), OF | NX);
    }

    #[test]
    fn fused_tiny_addend_past_maximum_sets_overflow() {
        let (mut cpu, mut bus) = setup();
        set_f64(&mut cpu, F1, f64::MAX);
        set_f64(&mut cpu, F2, 1.0);
        cpu.fregs[F3] = 1;

        run(
            &mut cpu,
            &mut bus,
            fused(FpOp::Madd, FpSize::D, F0, F1, F2, F3),
        );
        assert_eq!(fflags(&cpu), NX);

        clear_fflags(&mut cpu);
        run(
            &mut cpu,
            &mut bus,
            Op::Fp {
                op: FpOp::Madd,
                sz: FpSize::D,
                rd: F0,
                rs1: F1,
                rs2: F2,
                rs3: F3,
                rm: 3,
            },
        );

        assert_eq!(fflags(&cpu), OF | NX);
    }

    #[test]
    fn single_overflow_threshold_matches_one_ulp_past_maximum() {
        let max = single_wide(Single::largest());
        let previous = single_wide(Single::largest().next_down().value);
        let ulp = max.sub_r(previous, Round::NearestTiesToEven).value;
        let derived = max.add_r(ulp, Round::NearestTiesToEven).value;

        assert_eq!(derived, Double::from_bits(0x47f0_0000_0000_0000));
    }

    #[test]
    fn double_overflow_threshold_matches_one_ulp_past_maximum() {
        let max = double_wide(Double::largest());
        let previous = double_wide(Double::largest().next_down().value);
        let ulp = max.sub_r(previous, Round::NearestTiesToEven).value;
        let derived = max.add_r(ulp, Round::NearestTiesToEven).value;

        assert_eq!(derived, Quad::from_bits(0x43ff_u128 << 112));
    }

    #[test]
    fn fused_tiny_result_rounded_to_normal_sets_underflow() {
        let (mut cpu, mut bus) = setup();
        cpu.fregs[F1] = nan_box(0x0080_0000);
        cpu.fregs[F2] = nan_box(0x0A1F_0529);
        cpu.fregs[F3] = nan_box(0x007F_FFFF);
        let op = Op::Fp {
            op: FpOp::Madd,
            sz: FpSize::S,
            rd: F0,
            rs1: F1,
            rs2: F2,
            rs3: F3,
            rm: 3,
        };

        run(&mut cpu, &mut bus, op);

        assert_eq!(unbox_bits(cpu.fregs[F0]), 0x0080_0000);
        assert_eq!(fflags(&cpu), UF | NX);

        // The same exact fused result rounds to the maximum subnormal under
        // RTZ, which is tiny after rounding and therefore signals underflow.
        clear_fflags(&mut cpu);
        let op = Op::Fp {
            op: FpOp::Madd,
            sz: FpSize::S,
            rd: F0,
            rs1: F1,
            rs2: F2,
            rs3: F3,
            rm: 1,
        };
        run(&mut cpu, &mut bus, op);
        assert_eq!(unbox_bits(cpu.fregs[F0]), 0x007F_FFFF);
        assert_eq!(fflags(&cpu), UF | NX);
    }

    #[test]
    fn fmv_double_raw_bits() {
        let (mut cpu, mut bus) = setup();
        let bits = 0x4009_21FB_5444_2D18u64; // pi
        cpu.set_reg(X5, bits);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::MvFmtX, FpSize::D, F1, X5, 0, 0),
        );
        assert_eq!(cpu.fregs[F1], bits);
        run(
            &mut cpu,
            &mut bus,
            fp_op(FpOp::MvXFmt, FpSize::D, X6, F1, 0, 0),
        );
        assert_eq!(cpu.reg(X6), bits);
    }
}
