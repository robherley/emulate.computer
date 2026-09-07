//! Reusable half of the `emulate-computer` CLI: loading boot images, building
//! a [`Machine`](emulate_core::machine::Machine), attaching the relay-backed
//! network, and running an HTIF test ELF.
//!
//! The binary (`src/main.rs`) keeps only what needs a terminal: the clap front
//! end and the raw-mode console loop. The integration tests drive a guest
//! through this library instead of a PTY.
//!
//! [`capture`] adds the other direction: booting a guest headlessly through
//! [`headless`] and freezing it into a snapshot (see `docs/disk.md`).

pub mod capture;
pub mod elfload;
pub mod headless;
pub mod net;
pub mod run;
pub mod test;
