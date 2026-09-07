//! Exception and interrupt definitions (cause codes per the privileged spec).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    /// tval = misaligned target address
    InstructionAddressMisaligned(u64),
    /// tval = faulting physical-access virtual address
    InstructionAccessFault(u64),
    /// tval = raw instruction bits (0 if unavailable)
    IllegalInstruction(u64),
    /// tval = pc of the ebreak
    Breakpoint(u64),
    LoadAddressMisaligned(u64),
    LoadAccessFault(u64),
    StoreAddressMisaligned(u64),
    StoreAccessFault(u64),
    EnvironmentCallFromU,
    EnvironmentCallFromS,
    EnvironmentCallFromM,
    /// tval = faulting virtual address
    InstructionPageFault(u64),
    LoadPageFault(u64),
    StorePageFault(u64),
}

impl Exception {
    pub fn cause(&self) -> u64 {
        use Exception::*;
        match self {
            InstructionAddressMisaligned(_) => 0,
            InstructionAccessFault(_) => 1,
            IllegalInstruction(_) => 2,
            Breakpoint(_) => 3,
            LoadAddressMisaligned(_) => 4,
            LoadAccessFault(_) => 5,
            StoreAddressMisaligned(_) => 6,
            StoreAccessFault(_) => 7,
            EnvironmentCallFromU => 8,
            EnvironmentCallFromS => 9,
            EnvironmentCallFromM => 11,
            InstructionPageFault(_) => 12,
            LoadPageFault(_) => 13,
            StorePageFault(_) => 15,
        }
    }

    pub fn tval(&self) -> u64 {
        use Exception::*;
        match *self {
            InstructionAddressMisaligned(v)
            | InstructionAccessFault(v)
            | IllegalInstruction(v)
            | Breakpoint(v)
            | LoadAddressMisaligned(v)
            | LoadAccessFault(v)
            | StoreAddressMisaligned(v)
            | StoreAccessFault(v)
            | InstructionPageFault(v)
            | LoadPageFault(v)
            | StorePageFault(v) => v,
            EnvironmentCallFromU | EnvironmentCallFromS | EnvironmentCallFromM => 0,
        }
    }
}

/// Interrupt numbers (the value is the cause code / mip bit index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    SupervisorSoftware = 1,
    MachineSoftware = 3,
    SupervisorTimer = 5,
    MachineTimer = 7,
    SupervisorExternal = 9,
    MachineExternal = 11,
}

impl Interrupt {
    pub fn cause(&self) -> u64 {
        (1 << 63) | (*self as u64)
    }
    pub fn bit(&self) -> u64 {
        1 << (*self as u64)
    }
}

/// mip/mie bit masks.
pub mod irq {
    pub const SSIP: u64 = 1 << 1;
    pub const MSIP: u64 = 1 << 3;
    pub const STIP: u64 = 1 << 5;
    pub const MTIP: u64 = 1 << 7;
    pub const SEIP: u64 = 1 << 9;
    pub const MEIP: u64 = 1 << 11;
}
