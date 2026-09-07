//! ELF loading helpers (RV64 little-endian, PT_LOAD at p_paddr).

use object::elf::PT_LOAD;
use object::read::elf::{ElfFile64, ProgramHeader};
use object::{Object, ObjectSymbol};

use emulate_core::machine::Machine;

pub const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

pub fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == ELF_MAGIC
}

/// A parsed ELF ready to load.
///
/// `segments[i]` is `(physical address, file bytes)` and `memsz[i]` the
/// matching `p_memsz`; the two are parallel. The tail
/// `[paddr + file_bytes.len(), paddr + memsz)` is BSS the loader must zero
/// explicitly: RAM starts zeroed only for the first blob loaded, so a later
/// ELF whose `.bss` overlaps written memory would keep stale bytes.
#[derive(Debug)]
pub struct LoadedElf {
    pub entry: u64,
    pub segments: Vec<(u64, Vec<u8>)>,
    pub memsz: Vec<u64>,
}

impl LoadedElf {
    /// Iterate `(paddr, memsz, file_bytes)` for every loadable segment.
    pub fn loadable(&self) -> impl Iterator<Item = (u64, u64, &[u8])> {
        self.segments
            .iter()
            .zip(&self.memsz)
            .map(|((paddr, bytes), memsz)| (*paddr, *memsz, bytes.as_slice()))
    }
}

pub fn load_elf(data: &[u8]) -> Result<LoadedElf, String> {
    let file = ElfFile64::<object::Endianness>::parse(data)
        .map_err(|e| format!("not a valid 64-bit ELF: {e}"))?;
    let endian = file.endian();
    let mut segments = Vec::new();
    let mut memsz = Vec::new();
    for ph in file.elf_program_headers() {
        if ph.p_type(endian) != PT_LOAD {
            continue;
        }
        let seg_memsz = ph.p_memsz(endian);
        if seg_memsz == 0 {
            continue;
        }
        // `data` yields exactly p_filesz bytes, empty for a pure-BSS segment.
        let bytes = ph
            .data(endian, data)
            .map_err(|_| "ELF segment data out of bounds".to_string())?;
        if bytes.len() as u64 > seg_memsz {
            return Err(format!(
                "ELF segment at {:#x} has file size {:#x} > mem size {seg_memsz:#x}",
                ph.p_paddr(endian),
                bytes.len()
            ));
        }
        let paddr = ph.p_paddr(endian);
        paddr
            .checked_add(seg_memsz)
            .ok_or_else(|| format!("ELF PT_LOAD range at {paddr:#x} overflows"))?;
        segments.push((paddr, bytes.to_vec()));
        memsz.push(seg_memsz);
    }
    reject_overlapping_segments(&segments, &memsz)?;
    Ok(LoadedElf {
        entry: file.entry(),
        segments,
        memsz,
    })
}

/// Reject any two PT_LOAD segments whose `[p_paddr, p_paddr + p_memsz)` ranges
/// overlap: their in-memory footprints would clobber each other.
fn reject_overlapping_segments(segments: &[(u64, Vec<u8>)], memsz: &[u64]) -> Result<(), String> {
    let mut ranges: Vec<(u64, u64)> = segments
        .iter()
        .zip(memsz)
        .map(|((paddr, _), m)| (*paddr, *m))
        .collect();
    ranges.sort_by_key(|(paddr, _)| *paddr);
    for pair in ranges.windows(2) {
        let (a_start, a_len) = pair[0];
        let (b_start, b_len) = pair[1];
        let a_end = a_start
            .checked_add(a_len)
            .ok_or_else(|| format!("ELF PT_LOAD range at {a_start:#x} overflows"))?;
        let b_end = b_start
            .checked_add(b_len)
            .ok_or_else(|| format!("ELF PT_LOAD range at {b_start:#x} overflows"))?;
        if a_end > b_start {
            return Err(format!(
                "ELF PT_LOAD segments overlap: [{a_start:#x}..{a_end:#x}) and \
                 [{b_start:#x}..{b_end:#x})"
            ));
        }
    }
    Ok(())
}

#[expect(
    clippy::result_unit_err,
    reason = "segment loading preserves Machine::load_blob's compact hardware-fault result"
)]
pub fn load_segment(
    machine: &mut Machine,
    paddr: u64,
    memory_size: u64,
    file_bytes: &[u8],
) -> Result<(), ()> {
    let file_size = u64::try_from(file_bytes.len()).map_err(|_| ())?;
    let mut remaining = memory_size.checked_sub(file_size).ok_or(())?;
    let mut address = paddr.checked_add(file_size).ok_or(())?;
    machine.load_blob(paddr, file_bytes)?;

    const ZEROES: [u8; 4096] = [0; 4096];
    while remaining != 0 {
        let length = remaining.min(ZEROES.len() as u64) as usize;
        machine.load_blob(address, &ZEROES[..length])?;
        address = address.checked_add(length as u64).ok_or(())?;
        remaining -= length as u64;
    }
    Ok(())
}

/// Look up a symbol's address in the ELF symbol table.
pub fn symbol_addr(data: &[u8], name: &str) -> Option<u64> {
    let file = ElfFile64::<object::Endianness>::parse(data).ok()?;
    file.symbols()
        .find(|sym| sym.name() == Ok(name))
        .map(|sym| sym.address())
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulate_core::bus::DRAM_BASE;

    /// Check the loader against a real riscv-tests ELF, when one is built.
    #[test]
    fn loads_riscv_test_elf() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/riscv-tests/isa/rv64ui-p-add"
        );
        let Ok(data) = std::fs::read(path) else {
            eprintln!("skipping: {path} not built");
            return;
        };
        assert!(is_elf(&data));
        let elf = load_elf(&data).expect("parse");
        assert_eq!(elf.entry, 0x8000_0000, "riscv-tests entry at DRAM_BASE");
        assert!(!elf.segments.is_empty());
        for (paddr, bytes) in &elf.segments {
            assert!(*paddr >= 0x8000_0000, "segment paddr {paddr:#x} below DRAM");
            assert!(!bytes.is_empty());
        }
        for &m in &elf.memsz {
            assert!(m > 0);
        }
        assert_eq!(elf.segments.len(), elf.memsz.len());
        let tohost = symbol_addr(&data, "tohost").expect("tohost symbol");
        assert!(tohost >= 0x8000_0000);
    }

    /// Build a minimal little-endian RV64 ELF with the given PT_LOAD headers.
    /// Each header is `(p_paddr, p_filesz, p_memsz)`; file bytes are all 0xAA.
    fn synth_elf(entry: u64, loads: &[(u64, u64, u64)]) -> Vec<u8> {
        const EHSIZE: usize = 64;
        const PHENTSIZE: usize = 56;
        let phoff = EHSIZE;
        let ph_total = PHENTSIZE * loads.len();
        // Lay segment file data right after the program headers.
        let mut offsets = Vec::new();
        let mut cursor = (phoff + ph_total) as u64;
        for (_, filesz, _) in loads {
            offsets.push(cursor);
            cursor += *filesz;
        }
        let mut buf = vec![0u8; cursor as usize];
        // ELF header.
        buf[0..4].copy_from_slice(&ELF_MAGIC);
        buf[4] = 2; // ELFCLASS64
        buf[5] = 1; // ELFDATA2LSB
        buf[6] = 1; // EV_CURRENT
        buf[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        buf[18..20].copy_from_slice(&243u16.to_le_bytes()); // EM_RISCV
        buf[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
        buf[24..32].copy_from_slice(&entry.to_le_bytes()); // e_entry
        buf[32..40].copy_from_slice(&(phoff as u64).to_le_bytes()); // e_phoff
        buf[52..54].copy_from_slice(&(EHSIZE as u16).to_le_bytes()); // e_ehsize
        buf[54..56].copy_from_slice(&(PHENTSIZE as u16).to_le_bytes()); // e_phentsize
        buf[56..58].copy_from_slice(&(loads.len() as u16).to_le_bytes()); // e_phnum
                                                                          // Program headers.
        for (i, (paddr, filesz, memsz)) in loads.iter().enumerate() {
            let off = phoff + i * PHENTSIZE;
            buf[off..off + 4].copy_from_slice(&PT_LOAD.to_le_bytes()); // p_type
            buf[off + 4..off + 8].copy_from_slice(&4u32.to_le_bytes()); // p_flags = R
            buf[off + 8..off + 16].copy_from_slice(&offsets[i].to_le_bytes()); // p_offset
            buf[off + 16..off + 24].copy_from_slice(&paddr.to_le_bytes()); // p_vaddr
            buf[off + 24..off + 32].copy_from_slice(&paddr.to_le_bytes()); // p_paddr
            buf[off + 32..off + 40].copy_from_slice(&filesz.to_le_bytes()); // p_filesz
            buf[off + 40..off + 48].copy_from_slice(&memsz.to_le_bytes()); // p_memsz
            buf[off + 48..off + 56].copy_from_slice(&1u64.to_le_bytes()); // p_align
            let start = offsets[i] as usize;
            for b in &mut buf[start..start + *filesz as usize] {
                *b = 0xAA;
            }
        }
        buf
    }

    #[test]
    fn reports_bss_when_memsz_exceeds_filesz() {
        // One segment with 16 file bytes but a 4096-byte memory footprint.
        let data = synth_elf(0x8000_0000, &[(0x8000_0000, 16, 4096)]);
        let elf = load_elf(&data).expect("parse");
        assert_eq!(elf.segments.len(), 1);
        assert_eq!(elf.segments[0].0, 0x8000_0000);
        assert_eq!(elf.segments[0].1.len(), 16, "file bytes preserved");
        assert_eq!(elf.memsz[0], 4096, "memsz reported for BSS zero-fill");
        // The loadable() helper exposes the same triple.
        let (paddr, memsz, bytes) = elf.loadable().next().unwrap();
        assert_eq!((paddr, memsz, bytes.len()), (0x8000_0000, 4096, 16));
    }

    #[test]
    fn load_segment_explicitly_zeroes_bss() {
        let data = synth_elf(DRAM_BASE, &[(DRAM_BASE, 2, 6)]);
        let elf = load_elf(&data).expect("parse");
        let (paddr, memory_size, file_bytes) = elf.loadable().next().expect("load segment");
        let mut machine = Machine::new(8);
        machine
            .load_blob(DRAM_BASE, &[0xff; 6])
            .expect("prefill segment memory");

        load_segment(&mut machine, paddr, memory_size, file_bytes).expect("load segment");

        assert_eq!(machine.read_phys(DRAM_BASE, 2), Ok(0xaaaa));
        assert_eq!(machine.read_phys(DRAM_BASE + 2, 4), Ok(0));
    }

    #[test]
    fn load_segment_rejects_file_larger_than_memory_before_writing() {
        let mut machine = Machine::new(4);

        let result = load_segment(&mut machine, DRAM_BASE, 1, &[0xaa, 0xbb]);

        assert_eq!((result, machine.read_phys(DRAM_BASE, 2)), (Err(()), Ok(0)));
    }

    #[test]
    fn rejects_overlapping_pt_load_segments() {
        // Second segment's paddr lands inside the first segment's memsz range.
        let data = synth_elf(
            0x8000_0000,
            &[(0x8000_0000, 16, 0x2000), (0x8000_1000, 16, 16)],
        );
        let err = load_elf(&data).expect_err("overlap must be rejected");
        assert!(err.contains("overlap"), "unexpected error: {err}");
    }

    #[test]
    fn accepts_adjacent_non_overlapping_segments() {
        // Second segment starts exactly where the first ends: no overlap.
        let data = synth_elf(
            0x8000_0000,
            &[(0x8000_0000, 16, 0x1000), (0x8000_1000, 16, 0x1000)],
        );
        let elf = load_elf(&data).expect("adjacent segments are fine");
        assert_eq!(elf.segments.len(), 2);
    }
}
