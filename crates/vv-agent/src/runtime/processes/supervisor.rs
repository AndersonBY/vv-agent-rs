//! A Linux subreaper for one command, never for the embedding application.
//!
//! Only stack data and raw system calls are used in the post-fork supervisor.
//! It closes inherited application descriptors, reaps only its own children,
//! and emits a completion proof only after waitpid reports ECHILD.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;

#[derive(Debug)]
pub(super) struct ProcessTree {
    control: UnixStream,
    complete: bool,
}

impl ProcessTree {
    pub(super) fn prepare(command: &mut Command) -> std::io::Result<Self> {
        let (control, child_control) = UnixStream::pair()?;
        let control = above_stdio(control)?;
        let child_control = above_stdio(child_control)?;
        control.set_nonblocking(true)?;
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let close_limit = limit.rlim_max.min(i32::MAX as libc::rlim_t) as u32;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut old_action: libc::sigaction = std::mem::zeroed();
                let mut default_action: libc::sigaction = std::mem::zeroed();
                default_action.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut default_action.sa_mask);
                if libc::sigaction(libc::SIGCHLD, &default_action, &mut old_action) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Fork semantics without invoking the embedding application's
                // pthread_atfork callbacks a second time in the child.
                let root = fork_command();
                if root < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if root == 0 {
                    libc::close(child_control.as_raw_fd());
                    if libc::sigaction(libc::SIGCHLD, &old_action, std::ptr::null_mut()) != 0
                        || libc::setsid() < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    // std continues its normal exec/error-pipe path in the
                    // original command, preserving spawn failures and stdio.
                    return Ok(());
                }
                supervise(root, child_control.as_raw_fd(), close_limit)
            });
        }
        Ok(Self {
            control,
            complete: false,
        })
    }

    pub(super) fn request_stop(&mut self, force: bool) -> std::io::Result<()> {
        if !self.complete {
            self.control.write_all(if force { b"K" } else { b"T" })?;
        }
        Ok(())
    }

    pub(super) fn confirmed(&mut self) -> std::io::Result<bool> {
        if self.complete {
            return Ok(true);
        }
        let mut proof = [0];
        match self.control.read(&mut proof) {
            Ok(1) if proof == *b"D" => {
                self.complete = true;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
            _ => Err(std::io::Error::other(
                "command supervisor exited without confirming all descendants",
            )),
        }
    }
}

fn above_stdio(stream: UnixStream) -> std::io::Result<UnixStream> {
    if stream.as_raw_fd() > 2 {
        return Ok(stream);
    }
    // Command's stdio setup replaces these descriptor numbers before pre_exec.
    let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

// clone with only SIGCHLD is fork-equivalent. s390x swaps its first two args.
unsafe fn fork_command() -> libc::pid_t {
    #[cfg(target_arch = "s390x")]
    let (first, second) = (0usize, libc::SIGCHLD as usize);
    #[cfg(not(target_arch = "s390x"))]
    let (first, second) = (libc::SIGCHLD as usize, 0usize);
    libc::syscall(libc::SYS_clone, first, second, 0usize, 0usize, 0usize) as libc::pid_t
}

unsafe fn close_inherited_fds(control: i32, close_limit: u32) {
    // In particular, close std's exec-error pipe in the supervisor. Only the
    // command retains that pipe until exec, so Command::spawn remains truthful.
    let lower = libc::syscall(libc::SYS_close_range, 0u32, control as u32 - 1, 0u32);
    let upper = libc::syscall(libc::SYS_close_range, control as u32 + 1, u32::MAX, 0u32);
    if lower != 0 || upper != 0 {
        // Older Linux kernels do not implement close_range.
        for fd in 0..close_limit {
            if fd != control as u32 {
                libc::close(fd as i32);
            }
        }
    }
}

unsafe fn signal_children(root: libc::pid_t, root_reaped: bool, signal: i32) {
    // The original root is not reaped while its group is signaled. Its PID
    // therefore cannot have been reused for an unrelated process group.
    if !root_reaped {
        libc::kill(-root, signal);
    }
    let fd = libc::open(
        c"/proc/thread-self/children".as_ptr(),
        libc::O_RDONLY | libc::O_CLOEXEC,
    );
    if fd < 0 {
        return; // No false completion: only ECHILD can produce that proof.
    }
    let mut buffer = [0u8; 4096];
    let mut pid = 0i32;
    loop {
        let count = libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len());
        if count <= 0 {
            break;
        }
        for byte in &buffer[..count as usize] {
            if byte.is_ascii_digit() {
                pid = pid
                    .saturating_mul(10)
                    .saturating_add(i32::from(*byte - b'0'));
            } else if pid > 0 {
                // These are this supervisor's direct children. No wait occurs
                // between reading and killing; exited PIDs stay pinned zombies.
                libc::kill(pid, signal);
                pid = 0;
            }
        }
    }
    if pid > 0 {
        libc::kill(pid, signal);
    }
    libc::close(fd);
}

unsafe fn exit_like(status: i32) -> ! {
    if libc::WIFEXITED(status) {
        libc::_exit(libc::WEXITSTATUS(status));
    }
    if libc::WIFSIGNALED(status) {
        let signal = libc::WTERMSIG(status);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut());
        let mut unblocked: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblocked);
        libc::sigaddset(&mut unblocked, signal);
        libc::sigprocmask(libc::SIG_UNBLOCK, &unblocked, std::ptr::null_mut());
        libc::kill(libc::getpid(), signal);
    }
    libc::_exit(255);
}

unsafe fn supervise(root: libc::pid_t, control: i32, close_limit: u32) -> ! {
    close_inherited_fds(control, close_limit);
    let mut root_status = None;
    let mut stop_signal = 0;
    let mut connected = true;
    loop {
        if stop_signal != 0 {
            signal_children(root, root_status.is_some(), stop_signal);
        }
        loop {
            let mut status = 0;
            let pid = libc::waitpid(-1, &mut status, libc::WNOHANG);
            if pid == 0 {
                break;
            }
            if pid < 0 {
                let error = *libc::__errno_location();
                if error == libc::EINTR {
                    continue;
                }
                if error == libc::ECHILD {
                    if let Some(status) = root_status {
                        libc::send(control, b"D".as_ptr().cast(), 1, libc::MSG_NOSIGNAL);
                        exit_like(status);
                    }
                }
                libc::_exit(255); // Lost observation: deliberately no proof.
            }
            if pid == root {
                root_status = Some(status);
            }
        }
        let mut event = libc::pollfd {
            fd: control,
            events: libc::POLLIN,
            revents: 0,
        };
        if connected {
            if libc::poll(&mut event, 1, 10) > 0 {
                let mut requests = [0u8; 128];
                let count = libc::read(control, requests.as_mut_ptr().cast(), requests.len());
                if count == 0 {
                    connected = false;
                    stop_signal = libc::SIGKILL;
                } else if count > 0 {
                    for request in &requests[..count as usize] {
                        if *request == b'K' {
                            stop_signal = libc::SIGKILL;
                        } else if *request == b'T' && stop_signal != libc::SIGKILL {
                            stop_signal = libc::SIGTERM;
                        }
                    }
                }
            }
        } else {
            libc::poll(std::ptr::null_mut(), 0, 10);
        }
    }
}
