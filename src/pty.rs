//! Starting the program on a pseudo-terminal, and talking to it.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    master: File,
    pub pid: i32,
    /// Output that the kernel's buffer had no room for yet.
    pending: VecDeque<u8>,
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

/// The program to run when none was asked for: `$SHELL`, the password file, then sh.
fn default_shell() -> String {
    if let Some(s) = std::env::var("SHELL").ok().filter(|s| !s.is_empty()) {
        return s;
    }
    // SAFETY: getpwuid returns NULL or a pointer to static storage, which is read
    // at once. The process has only just started, so nothing else is calling it.
    let from_passwd = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            None
        } else {
            CStr::from_ptr((*pw).pw_shell).to_str().ok().map(str::to_string)
        }
    };
    if let Some(s) = from_passwd.filter(|s| !s.is_empty()) {
        return s;
    }
    "/bin/sh".to_string()
}

fn window_size(cols: usize, rows: usize, cell_w: u32, cell_h: u32) -> libc::winsize {
    libc::winsize {
        ws_row: rows as u16,
        ws_col: cols as u16,
        ws_xpixel: (cols as u32 * cell_w).min(u16::MAX as u32) as u16,
        ws_ypixel: (rows as u32 * cell_h).min(u16::MAX as u32) as u16,
    }
}

fn launch(argv: &[String], term: &str, slave: &OwnedFd) -> io::Result<Child> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave.try_clone()?))
        .env("TERM", term)
        .env("COLORTERM", "truecolor")
        .env_remove("COLUMNS")
        .env_remove("LINES")
        .env_remove("DESKTOP_STARTUP_ID")
        .env_remove("XDG_ACTIVATION_TOKEN");
    // SAFETY: setsid, ioctl and signal are async-signal-safe. Stdin is already
    // the terminal here, and this makes it the controlling one for a new session.
    unsafe {
        cmd.pre_exec(|| {
            // Ignored signals survive exec. Anything that started us in the
            // background would otherwise leave Ctrl+C dead in the shell.
            for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP, libc::SIGTSTP] {
                libc::signal(sig, libc::SIG_DFL);
            }
            check(libc::setsid())?;
            check(libc::ioctl(0, libc::TIOCSCTTY as _, 0))?;
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", argv[0])))
}

impl Pty {
    pub fn spawn(command: Option<&[String]>, term: &str, size: (usize, usize), cell: (u32, u32)) -> io::Result<Pty> {
        // SAFETY: plain libc calls with valid arguments. Each fd comes straight
        // from the call that opened it and is owned exactly once.
        let (master, slave) = unsafe {
            let fd = check(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC))?;
            let master = File::from_raw_fd(fd);
            check(libc::grantpt(fd))?;
            check(libc::unlockpt(fd))?;
            let mut name = [0 as libc::c_char; 128];
            check(libc::ptsname_r(fd, name.as_mut_ptr(), name.len()))?;
            let slave = check(libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC))?;
            let slave = OwnedFd::from_raw_fd(slave);
            let ws = window_size(size.0, size.1, cell.0, cell.1);
            check(libc::ioctl(fd, libc::TIOCSWINSZ, &ws))?;
            let flags = check(libc::fcntl(fd, libc::F_GETFL))?;
            check(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
            (master, slave)
        };

        let shell = default_shell();
        let child = match command {
            None => launch(&[shell], term, &slave),
            Some(argv) => launch(argv, term, &slave).or_else(|e| {
                // Like xterm, `-e "ls -l | less"` is a command line for the shell.
                // Only with spaces in it, so that a mistyped name gets a clear error.
                match argv {
                    [line] if line.contains(char::is_whitespace) => {
                        launch(&[shell.clone(), "-c".to_string(), line.clone()], term, &slave)
                    }
                    _ => Err(e),
                }
            }),
        }?;
        // The process is waited for by the main loop's child watch, not by `Child`.
        let pid = child.id() as i32;
        Ok(Pty { master, pid, pending: VecDeque::new() })
    }

    pub fn fd(&self) -> i32 {
        self.master.as_raw_fd()
    }

    pub fn resize(&self, cols: usize, rows: usize, cell: (u32, u32)) {
        let ws = window_size(cols, rows, cell.0, cell.1);
        // SAFETY: the fd is open and `ws` is a valid winsize.
        unsafe { libc::ioctl(self.fd(), libc::TIOCSWINSZ, &ws) };
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.master).read(buf)
    }

    /// Queue bytes for the program and send as much as fits right now.
    pub fn write(&mut self, bytes: &[u8]) {
        self.pending.extend(bytes);
        self.flush();
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Send queued bytes until done or until the kernel's buffer is full.
    pub fn flush(&mut self) {
        while !self.pending.is_empty() {
            let (chunk, _) = self.pending.as_slices();
            match (&self.master).write(chunk) {
                Ok(0) => break,
                Ok(n) => {
                    self.pending.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    // The program is gone; nobody will read this.
                    self.pending.clear();
                }
            }
        }
    }
}
