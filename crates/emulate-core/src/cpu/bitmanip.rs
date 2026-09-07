//! Zbb / Zbc / Zbkb / Zknd / Zkne / Zknh semantics.
//!
//! The decoder ([`super::decode`]) turns the OP / OP-32 / OP-IMM / OP-IMM-32
//! encodings that these extensions share with the base ISA into [`BitOp`],
//! [`BitUnaryOp`] and [`BitImmOp`]; this module is the pure arithmetic behind
//! them, so it can be unit-tested without a CPU.
//!
//! Semantics follow the Sail model in `riscv/sail-riscv`
//! (`model/extensions/K/{types_kext,zkn_insts}.sail`) and the ratified
//! Unprivileged ISA chapters for Zbb/Zbc/Zbkb. Every result is the full 64-bit
//! value written to `rd` (the `*W` forms sign-extend from bit 31 here).

/// Register-register forms living under the OP (0x33) and OP-32 (0x3b) opcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    // Zbb / Zbkb logic-with-negate
    Andn,
    Orn,
    Xnor,
    // Zbb min/max
    Max,
    Maxu,
    Min,
    Minu,
    // Zbb / Zbkb rotates
    Rol,
    Ror,
    Rolw,
    Rorw,
    // Zbkb packing (`zext.h` decodes as `packw rd, rs1, x0`)
    Pack,
    Packh,
    Packw,
    // Zbc carry-less multiply
    Clmul,
    Clmulh,
    Clmulr,
    // Zkne / Zknd
    Aes64es,
    Aes64esm,
    Aes64ds,
    Aes64dsm,
    Aes64ks2,
}

/// Unary forms: OP-IMM / OP-IMM-32 encodings with a fixed 12-bit `funct12`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitUnaryOp {
    // Zbb counting / extension / permutation
    Clz,
    Ctz,
    Cpop,
    Clzw,
    Ctzw,
    Cpopw,
    SextB,
    SextH,
    OrcB,
    Rev8,
    // Zbkb
    Brev8,
    // Zknd
    Aes64im,
    // Zknh
    Sha256sig0,
    Sha256sig1,
    Sha256sum0,
    Sha256sum1,
    Sha512sig0,
    Sha512sig1,
    Sha512sum0,
    Sha512sum1,
}

/// OP-IMM forms carrying a small immediate: a shift amount, or an AES round
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitImmOp {
    /// 6-bit shamt.
    Rori,
    /// 5-bit shamt, result sign-extended from bit 31.
    Roriw,
    /// 4-bit `rnum`, restricted to 0..=10 by the decoder.
    Aes64ks1i,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn bit_rr(kind: BitOp, a: u64, b: u64) -> u64 {
    match kind {
        BitOp::Andn => a & !b,
        BitOp::Orn => a | !b,
        BitOp::Xnor => !(a ^ b),
        BitOp::Max => (a as i64).max(b as i64) as u64,
        BitOp::Maxu => a.max(b),
        BitOp::Min => (a as i64).min(b as i64) as u64,
        BitOp::Minu => a.min(b),
        BitOp::Rol => a.rotate_left((b & 63) as u32),
        BitOp::Ror => a.rotate_right((b & 63) as u32),
        BitOp::Rolw => (a as u32).rotate_left((b & 31) as u32) as i32 as u64,
        BitOp::Rorw => (a as u32).rotate_right((b & 31) as u32) as i32 as u64,
        // pack: rs2[31:0] : rs1[31:0]
        BitOp::Pack => (a & 0xffff_ffff) | (b << 32),
        // packh: zero-extended rs2[7:0] : rs1[7:0]
        BitOp::Packh => (a & 0xff) | ((b & 0xff) << 8),
        // packw: sign-extended rs2[15:0] : rs1[15:0]
        BitOp::Packw => (((a & 0xffff) | ((b & 0xffff) << 16)) as u32) as i32 as u64,
        BitOp::Clmul => clmul(a, b),
        BitOp::Clmulh => clmulh(a, b),
        BitOp::Clmulr => clmulr(a, b),
        BitOp::Aes64es => aes64_round(a, b, Dir::Fwd, false),
        BitOp::Aes64esm => aes64_round(a, b, Dir::Fwd, true),
        BitOp::Aes64ds => aes64_round(a, b, Dir::Inv, false),
        BitOp::Aes64dsm => aes64_round(a, b, Dir::Inv, true),
        BitOp::Aes64ks2 => aes64ks2(a, b),
    }
}

pub fn bit_unary(kind: BitUnaryOp, a: u64) -> u64 {
    match kind {
        BitUnaryOp::Clz => a.leading_zeros() as u64,
        BitUnaryOp::Ctz => a.trailing_zeros() as u64,
        BitUnaryOp::Cpop => a.count_ones() as u64,
        BitUnaryOp::Clzw => (a as u32).leading_zeros() as u64,
        BitUnaryOp::Ctzw => (a as u32).trailing_zeros() as u64,
        BitUnaryOp::Cpopw => (a as u32).count_ones() as u64,
        BitUnaryOp::SextB => a as i8 as u64,
        BitUnaryOp::SextH => a as i16 as u64,
        BitUnaryOp::OrcB => orc_b(a),
        BitUnaryOp::Rev8 => a.swap_bytes(),
        BitUnaryOp::Brev8 => brev8(a),
        BitUnaryOp::Aes64im => {
            let lo = aes_mixcolumn_inv(a as u32);
            let hi = aes_mixcolumn_inv((a >> 32) as u32);
            (lo as u64) | ((hi as u64) << 32)
        }
        BitUnaryOp::Sha256sig0 => {
            let x = a as u32;
            (x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)) as i32 as u64
        }
        BitUnaryOp::Sha256sig1 => {
            let x = a as u32;
            (x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)) as i32 as u64
        }
        BitUnaryOp::Sha256sum0 => {
            let x = a as u32;
            (x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)) as i32 as u64
        }
        BitUnaryOp::Sha256sum1 => {
            let x = a as u32;
            (x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)) as i32 as u64
        }
        BitUnaryOp::Sha512sig0 => a.rotate_right(1) ^ a.rotate_right(8) ^ (a >> 7),
        BitUnaryOp::Sha512sig1 => a.rotate_right(19) ^ a.rotate_right(61) ^ (a >> 6),
        BitUnaryOp::Sha512sum0 => a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39),
        BitUnaryOp::Sha512sum1 => a.rotate_right(14) ^ a.rotate_right(18) ^ a.rotate_right(41),
    }
}

pub fn bit_imm(kind: BitImmOp, a: u64, imm: u32) -> u64 {
    match kind {
        BitImmOp::Rori => a.rotate_right(imm & 63),
        BitImmOp::Roriw => (a as u32).rotate_right(imm & 31) as i32 as u64,
        BitImmOp::Aes64ks1i => aes64ks1i(a, imm),
    }
}

// ---------------------------------------------------------------------------
// Zbb / Zbkb helpers
// ---------------------------------------------------------------------------

/// `orc.b`: every zero byte stays zero, every non-zero byte becomes 0xff.
///
/// SWAR: `(x & 0x7f..) + 0x7f..` carries into each byte's bit 7 iff the low
/// seven bits are non-zero; OR-ing `x` back in covers bit 7 itself. The final
/// multiply smears that flag bit across its byte (no carries: `1 * 0xff` fits).
fn orc_b(a: u64) -> u64 {
    const LOW7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    const ONES: u64 = 0x0101_0101_0101_0101;
    let flags = (((a & LOW7) + LOW7) | a) >> 7;
    (flags & ONES) * 0xff
}

/// `brev8`: reverse the bit order within each byte, byte positions unchanged.
fn brev8(a: u64) -> u64 {
    const M1: u64 = 0x5555_5555_5555_5555;
    const M2: u64 = 0x3333_3333_3333_3333;
    const M4: u64 = 0x0f0f_0f0f_0f0f_0f0f;
    let x = ((a & M1) << 1) | ((a >> 1) & M1);
    let x = ((x & M2) << 2) | ((x >> 2) & M2);
    ((x & M4) << 4) | ((x >> 4) & M4)
}

// ---------------------------------------------------------------------------
// Zbc carry-less multiply
// ---------------------------------------------------------------------------

/// The full 128-bit carry-less product; the three Zbc instructions are
/// different 64-bit windows onto it.
///
/// OpenSSL's GHASH issues several `clmul`s per 16 bytes of AEAD payload, so
/// this iterates once per *set* bit of `b` (`b &= b - 1` clears the lowest)
/// rather than once per bit position.
fn clmul_full(a: u64, b: u64) -> u128 {
    let wide = a as u128;
    let mut out = 0u128;
    let mut bits = b;
    while bits != 0 {
        out ^= wide << bits.trailing_zeros();
        bits &= bits - 1;
    }
    out
}

/// `clmul`: product bits 63:0.
fn clmul(a: u64, b: u64) -> u64 {
    clmul_full(a, b) as u64
}

/// `clmulh`: product bits 127:64.
fn clmulh(a: u64, b: u64) -> u64 {
    (clmul_full(a, b) >> 64) as u64
}

/// `clmulr`: product bits 126:63.
fn clmulr(a: u64, b: u64) -> u64 {
    (clmul_full(a, b) >> 63) as u64
}

// ---------------------------------------------------------------------------
// AES (Zknd / Zkne)
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const SBOX_FWD: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

#[rustfmt::skip]
const SBOX_INV: [u8; 256] = [
    0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3, 0xd7, 0xfb,
    0x7c, 0xe3, 0x39, 0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44, 0xc4, 0xde, 0xe9, 0xcb,
    0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2, 0x23, 0x3d, 0xee, 0x4c, 0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e,
    0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2, 0x76, 0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25,
    0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc, 0x5d, 0x65, 0xb6, 0x92,
    0x6c, 0x70, 0x48, 0x50, 0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d, 0x84,
    0x90, 0xd8, 0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06,
    0xd0, 0x2c, 0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13, 0x8a, 0x6b,
    0x3a, 0x91, 0x11, 0x41, 0x4f, 0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce, 0xf0, 0xb4, 0xe6, 0x73,
    0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85, 0xe2, 0xf9, 0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e,
    0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89, 0x6f, 0xb7, 0x62, 0x0e, 0xaa, 0x18, 0xbe, 0x1b,
    0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2, 0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd, 0x5a, 0xf4,
    0x1f, 0xdd, 0xa8, 0x33, 0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f,
    0x60, 0x51, 0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef,
    0xa0, 0xe0, 0x3b, 0x4d, 0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53, 0x99, 0x61,
    0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6, 0x26, 0xe1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0c, 0x7d,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Fwd,
    Inv,
}

/// GF(2^8) multiply-by-x (the AES `xtime`).
fn xt2(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0x00 }
}

fn xt3(x: u8) -> u8 {
    x ^ xt2(x)
}

/// Multiply an 8-bit field element by a 4-bit constant.
fn gfmul(x: u8, y: u8) -> u8 {
    let mut acc = 0u8;
    if y & 1 != 0 {
        acc ^= x;
    }
    if y & 2 != 0 {
        acc ^= xt2(x);
    }
    if y & 4 != 0 {
        acc ^= xt2(xt2(x));
    }
    if y & 8 != 0 {
        acc ^= xt2(xt2(xt2(x)));
    }
    acc
}

fn aes_mixcolumn_fwd(x: u32) -> u32 {
    let s = x.to_le_bytes();
    u32::from_le_bytes([
        xt2(s[0]) ^ xt3(s[1]) ^ s[2] ^ s[3],
        s[0] ^ xt2(s[1]) ^ xt3(s[2]) ^ s[3],
        s[0] ^ s[1] ^ xt2(s[2]) ^ xt3(s[3]),
        xt3(s[0]) ^ s[1] ^ s[2] ^ xt2(s[3]),
    ])
}

fn aes_mixcolumn_inv(x: u32) -> u32 {
    let s = x.to_le_bytes();
    u32::from_le_bytes([
        gfmul(s[0], 0xe) ^ gfmul(s[1], 0xb) ^ gfmul(s[2], 0xd) ^ gfmul(s[3], 0x9),
        gfmul(s[0], 0x9) ^ gfmul(s[1], 0xe) ^ gfmul(s[2], 0xb) ^ gfmul(s[3], 0xd),
        gfmul(s[0], 0xd) ^ gfmul(s[1], 0x9) ^ gfmul(s[2], 0xe) ^ gfmul(s[3], 0xb),
        gfmul(s[0], 0xb) ^ gfmul(s[1], 0xd) ^ gfmul(s[2], 0x9) ^ gfmul(s[3], 0xe),
    ])
}

fn getbyte(x: u64, i: u32) -> u8 {
    (x >> (8 * i)) as u8
}

/// `aes_rv64_shiftrows_fwd(rs2, rs1)` — byte selection from the spec's Sail.
fn shiftrows_fwd(rs2: u64, rs1: u64) -> u64 {
    u64::from_le_bytes([
        getbyte(rs1, 0),
        getbyte(rs1, 5),
        getbyte(rs2, 2),
        getbyte(rs2, 7),
        getbyte(rs1, 4),
        getbyte(rs2, 1),
        getbyte(rs2, 6),
        getbyte(rs1, 3),
    ])
}

/// `aes_rv64_shiftrows_inv(rs2, rs1)`.
fn shiftrows_inv(rs2: u64, rs1: u64) -> u64 {
    u64::from_le_bytes([
        getbyte(rs1, 0),
        getbyte(rs2, 5),
        getbyte(rs2, 2),
        getbyte(rs1, 7),
        getbyte(rs1, 4),
        getbyte(rs1, 1),
        getbyte(rs2, 6),
        getbyte(rs2, 3),
    ])
}

fn sbox_each_byte(x: u64, dir: Dir) -> u64 {
    let table = match dir {
        Dir::Fwd => &SBOX_FWD,
        Dir::Inv => &SBOX_INV,
    };
    let mut b = x.to_le_bytes();
    for byte in b.iter_mut() {
        *byte = table[*byte as usize];
    }
    u64::from_le_bytes(b)
}

/// `aes64es` / `aes64esm` / `aes64ds` / `aes64dsm`.  `a` is rs1, `b` is rs2.
fn aes64_round(a: u64, b: u64, dir: Dir, mix: bool) -> u64 {
    let sr = match dir {
        Dir::Fwd => shiftrows_fwd(b, a),
        Dir::Inv => shiftrows_inv(b, a),
    };
    let sb = sbox_each_byte(sr, dir);
    if !mix {
        return sb;
    }
    let mixer = match dir {
        Dir::Fwd => aes_mixcolumn_fwd,
        Dir::Inv => aes_mixcolumn_inv,
    };
    (mixer(sb as u32) as u64) | ((mixer((sb >> 32) as u32) as u64) << 32)
}

fn aes_subword_fwd(x: u32) -> u32 {
    let mut b = x.to_le_bytes();
    for byte in b.iter_mut() {
        *byte = SBOX_FWD[*byte as usize];
    }
    u32::from_le_bytes(b)
}

/// Round constant for `aes64ks1i`; `rnum` 0..=9 (10 is the AES-256 case, which
/// never reaches here).
fn aes_decode_rcon(rnum: u32) -> u32 {
    const RCON: [u32; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];
    RCON[rnum as usize]
}

fn aes64ks1i(a: u64, rnum: u32) -> u64 {
    let prev = (a >> 32) as u32;
    let subwords = aes_subword_fwd(prev);
    let result = if rnum == 0xa {
        subwords
    } else {
        subwords.rotate_right(8) ^ aes_decode_rcon(rnum)
    };
    (result as u64) | ((result as u64) << 32)
}

/// `aes64ks2 rd, rs1, rs2`: `a` is rs1, `b` is rs2.
fn aes64ks2(a: u64, b: u64) -> u64 {
    let w0 = ((a >> 32) as u32) ^ (b as u32);
    let w1 = w0 ^ ((b >> 32) as u32);
    (w0 as u64) | ((w1 as u64) << 32)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Zbb -------------------------------------------------------------

    #[test]
    fn zbb_logic_with_negate() {
        let (a, b) = (0xF0F0_F0F0_1234_5678u64, 0x00FF_00FF_0000_FFFFu64);
        assert_eq!(bit_rr(BitOp::Andn, a, b), a & !b);
        assert_eq!(bit_rr(BitOp::Orn, a, b), a | !b);
        assert_eq!(bit_rr(BitOp::Xnor, a, b), !(a ^ b));
        // andn with an all-ones mask clears everything.
        assert_eq!(bit_rr(BitOp::Andn, a, u64::MAX), 0);
        assert_eq!(bit_rr(BitOp::Orn, 0, 0), u64::MAX);
        assert_eq!(bit_rr(BitOp::Xnor, a, a), u64::MAX);
    }

    #[test]
    fn zbb_counting() {
        assert_eq!(bit_unary(BitUnaryOp::Clz, 0), 64);
        assert_eq!(bit_unary(BitUnaryOp::Clz, 1), 63);
        assert_eq!(bit_unary(BitUnaryOp::Clz, 1 << 63), 0);
        assert_eq!(bit_unary(BitUnaryOp::Ctz, 0), 64);
        assert_eq!(bit_unary(BitUnaryOp::Ctz, 1 << 40), 40);
        assert_eq!(bit_unary(BitUnaryOp::Cpop, u64::MAX), 64);
        assert_eq!(bit_unary(BitUnaryOp::Cpop, 0xdead_beef_dead_beef), 48);

        // The *W forms look only at bits 31:0.
        assert_eq!(bit_unary(BitUnaryOp::Clzw, 0xffff_ffff_0000_0000), 32);
        assert_eq!(bit_unary(BitUnaryOp::Clzw, 0xffff_ffff_0000_0001), 31);
        assert_eq!(bit_unary(BitUnaryOp::Ctzw, 0), 32);
        assert_eq!(bit_unary(BitUnaryOp::Ctzw, 0x0000_0000_8000_0000), 31);
        assert_eq!(bit_unary(BitUnaryOp::Cpopw, u64::MAX), 32);
    }

    #[test]
    fn zbb_minmax() {
        let neg = (-5i64) as u64;
        assert_eq!(bit_rr(BitOp::Max, neg, 3), 3);
        assert_eq!(bit_rr(BitOp::Min, neg, 3), neg);
        assert_eq!(bit_rr(BitOp::Maxu, neg, 3), neg); // unsigned: huge
        assert_eq!(bit_rr(BitOp::Minu, neg, 3), 3);
    }

    #[test]
    fn zbb_sign_and_zero_extend() {
        assert_eq!(bit_unary(BitUnaryOp::SextB, 0xff), u64::MAX);
        assert_eq!(bit_unary(BitUnaryOp::SextB, 0x7f), 0x7f);
        assert_eq!(
            bit_unary(BitUnaryOp::SextH, 0xabcd_8000),
            0xffff_ffff_ffff_8000
        );
        assert_eq!(bit_unary(BitUnaryOp::SextH, 0x1234), 0x1234);
        // zext.h is `packw rd, rs1, x0`.
        assert_eq!(bit_rr(BitOp::Packw, 0xdead_beef_1234_abcd, 0), 0xabcd);
    }

    #[test]
    fn zbb_rotates() {
        let a = 0x0123_4567_89ab_cdefu64;
        assert_eq!(bit_rr(BitOp::Rol, a, 8), 0x2345_6789_abcd_ef01);
        assert_eq!(bit_rr(BitOp::Ror, a, 8), 0xef01_2345_6789_abcd);
        // Shift amounts are masked to 6 bits.
        assert_eq!(bit_rr(BitOp::Rol, a, 64 + 8), bit_rr(BitOp::Rol, a, 8));
        assert_eq!(bit_imm(BitImmOp::Rori, a, 8), bit_rr(BitOp::Ror, a, 8));
        assert_eq!(bit_imm(BitImmOp::Rori, a, 0), a);

        // *W forms rotate within 32 bits, then sign-extend from bit 31.
        assert_eq!(bit_rr(BitOp::Rolw, 0x8000_0001, 1), 3);
        assert_eq!(bit_rr(BitOp::Rorw, 0x0000_0003, 1), 0xffff_ffff_8000_0001);
        assert_eq!(
            bit_imm(BitImmOp::Roriw, 0x0000_0003, 1),
            0xffff_ffff_8000_0001
        );
        // The upper half of rs1 is ignored by the *W forms.
        assert_eq!(bit_rr(BitOp::Rolw, 0xdead_beef_8000_0001, 1), 3);
        assert_eq!(bit_rr(BitOp::Rolw, a, 32 + 4), bit_rr(BitOp::Rolw, a, 4));
    }

    #[test]
    fn zbb_orc_and_rev8() {
        assert_eq!(bit_unary(BitUnaryOp::OrcB, 0), 0);
        assert_eq!(
            bit_unary(BitUnaryOp::OrcB, 0x0100_0002_0000_0080),
            0xff00_00ff_0000_00ff
        );
        assert_eq!(bit_unary(BitUnaryOp::OrcB, u64::MAX), u64::MAX);
        assert_eq!(
            bit_unary(BitUnaryOp::Rev8, 0x0123_4567_89ab_cdef),
            0xefcd_ab89_6745_2301
        );
        // rev8 is an involution.
        let a = 0xdead_beef_cafe_f00du64;
        assert_eq!(
            bit_unary(BitUnaryOp::Rev8, bit_unary(BitUnaryOp::Rev8, a)),
            a
        );
    }

    #[test]
    fn zbkb_brev8_and_pack() {
        // 0x01 -> 0x80 within its byte; byte positions do not move.
        assert_eq!(bit_unary(BitUnaryOp::Brev8, 0x0000_0000_0000_0001), 0x80);
        assert_eq!(
            bit_unary(BitUnaryOp::Brev8, 0x0102_0400_0000_0000),
            0x8040_2000_0000_0000
        );
        let a = 0x1234_5678_9abc_def0u64;
        assert_eq!(
            bit_unary(BitUnaryOp::Brev8, bit_unary(BitUnaryOp::Brev8, a)),
            a
        );

        assert_eq!(
            bit_rr(BitOp::Pack, 0xffff_ffff_1111_2222, 0xeeee_eeee_3333_4444),
            0x3333_4444_1111_2222
        );
        assert_eq!(bit_rr(BitOp::Packh, 0xff12, 0xff34), 0x3412);
        assert_eq!(
            bit_rr(BitOp::Packw, 0xffff_ffff_1111_2222, 0xffff_ffff_3333_4444),
            0x4444_2222
        );
    }

    /// `orc.b` and `brev8` are implemented with SWAR tricks; check them
    /// against the obvious byte-at-a-time model over pseudo-random inputs.
    #[test]
    fn zbb_zbkb_swar_helpers_match_naive_model() {
        fn orc_b_naive(a: u64) -> u64 {
            let mut out = 0u64;
            for i in 0..8 {
                if (a >> (8 * i)) & 0xff != 0 {
                    out |= 0xffu64 << (8 * i);
                }
            }
            out
        }
        fn brev8_naive(a: u64) -> u64 {
            let mut out = 0u64;
            for i in 0..8 {
                let b = ((a >> (8 * i)) & 0xff) as u8;
                out |= (b.reverse_bits() as u64) << (8 * i);
            }
            out
        }

        // A cheap xorshift so the inputs cover every byte pattern class.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            assert_eq!(orc_b(x), orc_b_naive(x), "orc.b {x:#018x}");
            assert_eq!(brev8(x), brev8_naive(x), "brev8 {x:#018x}");
        }
        // Boundary bytes: every single-byte value in the low byte, and the
        // all-zero / all-ones extremes.
        for byte in 0..=255u64 {
            let v = 0xff00_00ff_0000_0000 | byte;
            assert_eq!(orc_b(v), orc_b_naive(v));
            assert_eq!(brev8(v), brev8_naive(v));
        }
        assert_eq!(orc_b(0), 0);
        assert_eq!(orc_b(u64::MAX), u64::MAX);
        assert_eq!(brev8(0), 0);
        assert_eq!(brev8(u64::MAX), u64::MAX);
    }

    // -- Zbc -------------------------------------------------------------

    /// Reference 128-bit carry-less product.
    fn clmul128(a: u64, b: u64) -> u128 {
        let mut out = 0u128;
        for i in 0..64 {
            if (b >> i) & 1 != 0 {
                out ^= (a as u128) << i;
            }
        }
        out
    }

    #[test]
    fn zbc_clmul_matches_128_bit_reference() {
        let vals = [
            0u64,
            1,
            2,
            0xffff_ffff_ffff_ffff,
            0x8000_0000_0000_0000,
            0x0123_4567_89ab_cdef,
            0xdead_beef_cafe_babe,
            0xa5a5_a5a5_5a5a_5a5a,
        ];
        for &a in &vals {
            for &b in &vals {
                let full = clmul128(a, b);
                assert_eq!(
                    bit_rr(BitOp::Clmul, a, b),
                    full as u64,
                    "clmul {a:#x} {b:#x}"
                );
                assert_eq!(
                    bit_rr(BitOp::Clmulh, a, b),
                    (full >> 64) as u64,
                    "clmulh {a:#x} {b:#x}"
                );
                assert_eq!(
                    bit_rr(BitOp::Clmulr, a, b),
                    (full >> 63) as u64,
                    "clmulr {a:#x} {b:#x}"
                );
            }
        }
        // Pseudo-random operands, since the implementation iterates over set
        // bits rather than bit positions.
        let (mut x, mut y) = (0x2545_f491_4f6c_dd1du64, 0x9e37_79b9_7f4a_7c15u64);
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            y = y.wrapping_mul(6364136223846793005).wrapping_add(1);
            let full = clmul128(x, y);
            assert_eq!(bit_rr(BitOp::Clmul, x, y), full as u64);
            assert_eq!(bit_rr(BitOp::Clmulh, x, y), (full >> 64) as u64);
            assert_eq!(bit_rr(BitOp::Clmulr, x, y), (full >> 63) as u64);
        }

        // clmul by 1 is the identity; squaring spreads bits apart.
        assert_eq!(bit_rr(BitOp::Clmul, 0x1234_5678, 1), 0x1234_5678);
        assert_eq!(bit_rr(BitOp::Clmul, 0b1011, 0b1011), 0b100_0101);
    }

    // -- Zknh ------------------------------------------------------------

    #[test]
    fn zknh_sha256_matches_fips180_round_functions() {
        // FIPS 180-4 §4.1.2 definitions, computed independently here.
        fn big_sigma0(x: u32) -> u32 {
            x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
        }
        fn big_sigma1(x: u32) -> u32 {
            x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
        }
        fn small_sigma0(x: u32) -> u32 {
            x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
        }
        fn small_sigma1(x: u32) -> u32 {
            x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
        }

        for &x in &[0u32, 1, 0x6a09_e667, 0xdead_beef, u32::MAX] {
            // Upper bits of rs1 must be ignored; results sign-extend from 31.
            let src = 0xdead_beef_0000_0000u64 | x as u64;
            assert_eq!(
                bit_unary(BitUnaryOp::Sha256sum0, src),
                big_sigma0(x) as i32 as u64
            );
            assert_eq!(
                bit_unary(BitUnaryOp::Sha256sum1, src),
                big_sigma1(x) as i32 as u64
            );
            assert_eq!(
                bit_unary(BitUnaryOp::Sha256sig0, src),
                small_sigma0(x) as i32 as u64
            );
            assert_eq!(
                bit_unary(BitUnaryOp::Sha256sig1, src),
                small_sigma1(x) as i32 as u64
            );
        }
    }

    /// The full SHA-256 message schedule + compression built from the Zknh
    /// primitives must reproduce the FIPS 180-4 digest of "abc".
    #[test]
    fn zknh_sha256_full_hash_of_abc() {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        // Padded single block for the message "abc".
        let mut w = [0u32; 64];
        w[0] = 0x6162_6380;
        w[15] = 24;
        // Message schedule via sha256sig0 / sha256sig1.
        for t in 16..64 {
            let s0 = bit_unary(BitUnaryOp::Sha256sig0, w[t - 15] as u64) as u32;
            let s1 = bit_unary(BitUnaryOp::Sha256sig1, w[t - 2] as u64) as u32;
            w[t] = s1
                .wrapping_add(w[t - 7])
                .wrapping_add(s0)
                .wrapping_add(w[t - 16]);
        }
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for t in 0..64 {
            // Compression via sha256sum0 / sha256sum1.
            let s1 = bit_unary(BitUnaryOp::Sha256sum1, e as u64) as u32;
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[t])
                .wrapping_add(w[t]);
            let s0 = bit_unary(BitUnaryOp::Sha256sum0, a as u64) as u32;
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].iter().enumerate() {
            h[i] = h[i].wrapping_add(*v);
        }
        // FIPS 180-4 SHA-256("abc")
        assert_eq!(
            h,
            [
                0xba7816bf, 0x8f01cfea, 0x414140de, 0x5dae2223, 0xb00361a3, 0x96177a9c, 0xb410ff61,
                0xf20015ad
            ]
        );
    }

    /// SHA-512 primitives, checked by hashing "abc" with them.
    #[test]
    fn zknh_sha512_full_hash_of_abc() {
        #[rustfmt::skip]
        const K: [u64; 80] = [
            0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
            0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
            0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
            0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
            0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
            0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
            0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
            0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
            0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
            0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
            0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
            0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
            0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
            0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
            0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
            0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
            0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
            0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
            0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
            0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
        ];
        let mut w = [0u64; 80];
        w[0] = 0x6162_6380_0000_0000;
        w[15] = 24;
        for t in 16..80 {
            let s0 = bit_unary(BitUnaryOp::Sha512sig0, w[t - 15]);
            let s1 = bit_unary(BitUnaryOp::Sha512sig1, w[t - 2]);
            w[t] = s1
                .wrapping_add(w[t - 7])
                .wrapping_add(s0)
                .wrapping_add(w[t - 16]);
        }
        let mut h: [u64; 8] = [
            0x6a09e667f3bcc908,
            0xbb67ae8584caa73b,
            0x3c6ef372fe94f82b,
            0xa54ff53a5f1d36f1,
            0x510e527fade682d1,
            0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b,
            0x5be0cd19137e2179,
        ];
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for (t, k) in K.iter().enumerate() {
            let s1 = bit_unary(BitUnaryOp::Sha512sum1, e);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(w[t]);
            let s0 = bit_unary(BitUnaryOp::Sha512sum0, a);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].iter().enumerate() {
            h[i] = h[i].wrapping_add(*v);
        }
        // FIPS 180-4 SHA-512("abc")
        assert_eq!(
            h,
            [
                0xddaf35a193617aba,
                0xcc417349ae204131,
                0x12e6fa4e89a97ea2,
                0x0a9eeee64b55d39a,
                0x2192992a274fc1a8,
                0x36ba3c23a3feebbd,
                0x454d4423643ce80e,
                0x2a9ac94fa54ca49f,
            ]
        );
    }

    // -- Zknd / Zkne ------------------------------------------------------

    /// AES-128 key expansion built from `aes64ks1i` + `aes64ks2`, exactly as
    /// the spec's software note (and OpenSSL's Zkn path) sequences them.
    /// Returns 11 round keys as (low 64, high 64) pairs.
    fn aes128_key_schedule(key: [u8; 16]) -> [(u64, u64); 11] {
        let mut rk = [(0u64, 0u64); 11];
        rk[0] = (
            u64::from_le_bytes(key[0..8].try_into().unwrap()),
            u64::from_le_bytes(key[8..16].try_into().unwrap()),
        );
        for i in 0..10 {
            let (rk0, rk1) = rk[i];
            let t = bit_imm(BitImmOp::Aes64ks1i, rk1, i as u32);
            let n0 = bit_rr(BitOp::Aes64ks2, t, rk0);
            let n1 = bit_rr(BitOp::Aes64ks2, n0, rk1);
            rk[i + 1] = (n0, n1);
        }
        rk
    }

    fn aes128_encrypt_block(rk: &[(u64, u64); 11], pt: [u8; 16]) -> [u8; 16] {
        let mut s0 = u64::from_le_bytes(pt[0..8].try_into().unwrap()) ^ rk[0].0;
        let mut s1 = u64::from_le_bytes(pt[8..16].try_into().unwrap()) ^ rk[0].1;
        for round_key in rk.iter().take(10).skip(1) {
            // aes64esm t2, t0, t1 / aes64esm t3, t1, t0
            let n0 = bit_rr(BitOp::Aes64esm, s0, s1) ^ round_key.0;
            let n1 = bit_rr(BitOp::Aes64esm, s1, s0) ^ round_key.1;
            s0 = n0;
            s1 = n1;
        }
        let n0 = bit_rr(BitOp::Aes64es, s0, s1) ^ rk[10].0;
        let n1 = bit_rr(BitOp::Aes64es, s1, s0) ^ rk[10].1;
        let mut out = [0u8; 16];
        out[0..8].copy_from_slice(&n0.to_le_bytes());
        out[8..16].copy_from_slice(&n1.to_le_bytes());
        out
    }

    fn aes128_decrypt_block(rk: &[(u64, u64); 11], ct: [u8; 16]) -> [u8; 16] {
        // Equivalent inverse cipher: the middle round keys go through
        // InvMixColumns, which is exactly what aes64im provides.
        let mut dk = *rk;
        for k in dk.iter_mut().take(10).skip(1) {
            k.0 = bit_unary(BitUnaryOp::Aes64im, k.0);
            k.1 = bit_unary(BitUnaryOp::Aes64im, k.1);
        }
        let mut s0 = u64::from_le_bytes(ct[0..8].try_into().unwrap()) ^ rk[10].0;
        let mut s1 = u64::from_le_bytes(ct[8..16].try_into().unwrap()) ^ rk[10].1;
        for r in (1..10).rev() {
            let n0 = bit_rr(BitOp::Aes64dsm, s0, s1) ^ dk[r].0;
            let n1 = bit_rr(BitOp::Aes64dsm, s1, s0) ^ dk[r].1;
            s0 = n0;
            s1 = n1;
        }
        let n0 = bit_rr(BitOp::Aes64ds, s0, s1) ^ rk[0].0;
        let n1 = bit_rr(BitOp::Aes64ds, s1, s0) ^ rk[0].1;
        let mut out = [0u8; 16];
        out[0..8].copy_from_slice(&n0.to_le_bytes());
        out[8..16].copy_from_slice(&n1.to_le_bytes());
        out
    }

    #[test]
    fn zkn_aes_sboxes_are_inverses() {
        for i in 0..256usize {
            assert_eq!(SBOX_INV[SBOX_FWD[i] as usize] as usize, i);
            assert_eq!(SBOX_FWD[SBOX_INV[i] as usize] as usize, i);
        }
    }

    #[test]
    fn zkn_aes_mixcolumns_round_trips() {
        for x in [0u32, 1, 0xdb135345, 0xf20a225c, 0xffff_ffff, 0x0123_4567] {
            assert_eq!(aes_mixcolumn_inv(aes_mixcolumn_fwd(x)), x);
        }
        // FIPS-197 §4.3 worked MixColumns example: 0xdb135345 -> 0x8e4da1bc
        // (bytes given most-significant-first in the document; our words are
        // little-endian byte order, so both are byte-reversed here).
        assert_eq!(aes_mixcolumn_fwd(0x4553_13db), 0xbca1_4d8e);
    }

    /// FIPS-197 Appendix A.1 AES-128 key expansion.
    #[test]
    fn zkn_aes128_key_schedule_matches_fips197() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let rk = aes128_key_schedule(key);
        // w[4..8] from FIPS-197 Appendix A.1. The document prints each word
        // most-significant-byte-first; registers hold them little-endian.
        let w4 = (rk[1].0 as u32).swap_bytes();
        let w5 = ((rk[1].0 >> 32) as u32).swap_bytes();
        let w6 = (rk[1].1 as u32).swap_bytes();
        let w7 = ((rk[1].1 >> 32) as u32).swap_bytes();
        assert_eq!(
            [w4, w5, w6, w7],
            [0xa0fafe17, 0x88542cb1, 0x23a33939, 0x2a6c7605]
        );
        // Last round key w[40..44].
        let w40 = (rk[10].0 as u32).swap_bytes();
        let w43 = ((rk[10].1 >> 32) as u32).swap_bytes();
        assert_eq!(w40, 0xd014f9a8);
        assert_eq!(w43, 0xb6630ca6);
    }

    /// FIPS-197 Appendix B / C.1 AES-128 known-answer test, driven entirely
    /// through the Zkne/Zknd instruction semantics.
    #[test]
    fn zkn_aes128_fips197_known_answer() {
        // Appendix B: key 2b7e..., plaintext 3243f6a8..., ciphertext 3925841d...
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let pt = [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d, 0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37,
            0x07, 0x34,
        ];
        let expect = [
            0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb, 0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a,
            0x0b, 0x32,
        ];
        let rk = aes128_key_schedule(key);
        assert_eq!(aes128_encrypt_block(&rk, pt), expect);
        assert_eq!(aes128_decrypt_block(&rk, expect), pt);

        // Appendix C.1: key 000102...0f, plaintext 00112233...ff.
        let key2 = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let pt2 = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let expect2 = [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
            0xc5, 0x5a,
        ];
        let rk2 = aes128_key_schedule(key2);
        assert_eq!(aes128_encrypt_block(&rk2, pt2), expect2);
        assert_eq!(aes128_decrypt_block(&rk2, expect2), pt2);
    }

    #[test]
    fn zkn_aes64ks1i_rnum_ten_skips_rotate_and_rcon() {
        let src = 0x0123_4567_89ab_cdefu64;
        let sub = aes_subword_fwd((src >> 32) as u32);
        assert_eq!(
            bit_imm(BitImmOp::Aes64ks1i, src, 0xa),
            (sub as u64) | ((sub as u64) << 32)
        );
        let r0 = sub.rotate_right(8) ^ 1;
        assert_eq!(
            bit_imm(BitImmOp::Aes64ks1i, src, 0),
            (r0 as u64) | ((r0 as u64) << 32)
        );
    }
}
