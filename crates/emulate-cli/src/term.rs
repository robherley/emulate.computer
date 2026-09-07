//! Raw-terminal handling for the interactive console (libc termios).

use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicI32, Ordering};

static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn record_signal(signal: libc::c_int) {
    let _ = PENDING_SIGNAL.compare_exchange(0, signal, Ordering::Relaxed, Ordering::Relaxed);
}

pub fn pending_signal() -> Option<libc::c_int> {
    match PENDING_SIGNAL.load(Ordering::Relaxed) {
        0 => None,
        signal => Some(signal),
    }
}

fn enable_nonblocking(fd: libc::c_int) -> libc::c_int {
    let original = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, original | libc::O_NONBLOCK) };
    }
    original
}

fn restore_file_flags(fd: libc::c_int, original: &mut libc::c_int) {
    if *original >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, *original) };
        *original = -1;
    }
}

/// RAII guard that makes stdin raw and non-blocking. Signal handlers only
/// record the signal; normal unwinding restores termios and descriptor flags.
pub struct RawTerm {
    orig_termios: Option<libc::termios>,
    orig_flags: libc::c_int,
    old_sigint: libc::sighandler_t,
    old_sigterm: libc::sighandler_t,
    handlers_installed: bool,
}

impl RawTerm {
    pub fn enable() -> RawTerm {
        let fd = libc::STDIN_FILENO;
        let is_tty = unsafe { libc::isatty(fd) } == 1;
        PENDING_SIGNAL.store(0, Ordering::Relaxed);

        let old_sigint = unsafe {
            libc::signal(
                libc::SIGINT,
                record_signal as *const () as libc::sighandler_t,
            )
        };
        let old_sigterm = unsafe {
            libc::signal(
                libc::SIGTERM,
                record_signal as *const () as libc::sighandler_t,
            )
        };

        // Always make stdin non-blocking (works for pipes too).
        let orig_flags = enable_nonblocking(fd);

        if !is_tty {
            return RawTerm {
                orig_termios: None,
                orig_flags,
                old_sigint,
                old_sigterm,
                handlers_installed: true,
            };
        }

        let mut orig = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, orig.as_mut_ptr()) } != 0 {
            return RawTerm {
                orig_termios: None,
                orig_flags,
                old_sigint,
                old_sigterm,
                handlers_installed: true,
            };
        }
        let orig = unsafe { orig.assume_init() };

        let mut raw = orig;
        unsafe { libc::cfmakeraw(&mut raw) };
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &raw);
        }
        RawTerm {
            orig_termios: Some(orig),
            orig_flags,
            old_sigint,
            old_sigterm,
            handlers_installed: true,
        }
    }

    pub fn restore(&mut self) {
        let fd = libc::STDIN_FILENO;
        if let Some(orig) = self.orig_termios.take() {
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &orig);
            }
        }
        restore_file_flags(fd, &mut self.orig_flags);
        if self.handlers_installed {
            unsafe {
                libc::signal(libc::SIGINT, self.old_sigint);
                libc::signal(libc::SIGTERM, self.old_sigterm);
            }
            self.handlers_installed = false;
        }
    }
}

impl Drop for RawTerm {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Wait up to `timeout_ms` for stdin to become readable.
pub fn poll_stdin(timeout_ms: i32) -> bool {
    let mut fds = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let r = unsafe { libc::poll(&mut fds, 1, timeout_ms) };
    r > 0 && (fds.revents & libc::POLLIN) != 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinRead {
    Data(usize),
    WouldBlock,
    Eof,
    Error(libc::c_int),
}

fn classify_read_result(result: isize, error: libc::c_int) -> StdinRead {
    if result > 0 {
        StdinRead::Data(result as usize)
    } else if result == 0 {
        StdinRead::Eof
    } else if error == libc::EAGAIN || error == libc::EWOULDBLOCK || error == libc::EINTR {
        StdinRead::WouldBlock
    } else {
        StdinRead::Error(error)
    }
}

/// Read from non-blocking stdin, preserving the distinction between an empty
/// pipe and a descriptor that simply has no data available yet.
pub fn read_stdin(buf: &mut [u8]) -> StdinRead {
    let n = unsafe {
        libc::read(
            libc::STDIN_FILENO,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    let error = if n < 0 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    } else {
        0
    };
    classify_read_result(n, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length_read_is_eof() {
        assert_eq!(classify_read_result(0, 0), StdinRead::Eof);
    }

    #[test]
    fn eagain_is_not_eof() {
        assert_eq!(
            classify_read_result(-1, libc::EAGAIN),
            StdinRead::WouldBlock
        );
    }

    #[test]
    fn successful_read_reports_its_length() {
        assert_eq!(classify_read_result(17, 0), StdinRead::Data(17));
    }

    #[test]
    fn nonblocking_setup_restores_original_descriptor_flags() {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let original = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };

        let mut saved = enable_nonblocking(fds[0]);
        let enabled = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };
        restore_file_flags(fds[0], &mut saved);
        let restored = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        assert_ne!(enabled & libc::O_NONBLOCK, 0);
        assert_eq!(restored, original);
    }
}
