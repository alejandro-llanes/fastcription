//! Running a subprocess with a deadline.
//!
//! Nothing here is specific to voxtype: it is the same shape `fc-asr` uses for
//! `voxtype transcribe` and `fc-audio` uses for `pactl`, duplicated rather
//! than shared through a seventh workspace crate — thirty lines is a smaller
//! cost than a crate boundary.
//!
//! Why any of this is needed: `service_status` and the `voxtype info` probes run
//! on the interface thread, so a `systemctl` or a `voxtype` that never answers
//! freezes the window. `systemctl --user` in particular waits on the user bus,
//! which a restarting `systemd --user` does not answer.
//!
//! **The timeout bounds the whole call, not just the wait for the child.**
//! `kill` only reaches the direct child; a descendant that inherited the
//! stdout/stderr pipes keeps their write ends open, so the pipe never sees EOF.
//! The readers therefore hand their buffers over a channel and are waited on
//! with whatever is left of the budget, then abandoned — each still exits and is
//! reaped by the OS the moment its pipe closes.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How often the child is polled for having exited. Short enough that a fast
/// command is not noticeably delayed, long enough not to spin a core.
const POLL: Duration = Duration::from_millis(10);

/// Runs `command` to completion, or gives up after `timeout` and returns
/// `Ok(None)`. `stdin` is closed, so a child that reads it sees EOF rather than
/// waiting on a terminal that is not there.
pub(crate) fn output_within(
    command: &mut Command,
    timeout: Duration,
) -> std::io::Result<Option<Output>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                std::thread::sleep(POLL);
            }
        }
    };

    let Some(stdout) = collect_within(&stdout, timeout.saturating_sub(start.elapsed())) else {
        return Ok(None);
    };
    let Some(stderr) = collect_within(&stderr, timeout.saturating_sub(start.elapsed())) else {
        return Ok(None);
    };
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// `None` means the pipe never reached EOF in time. A disconnected channel (the
/// reader thread panicked) reads as an empty pipe, which the caller's own parse
/// then rejects.
fn collect_within(rx: &mpsc::Receiver<Vec<u8>>, budget: Duration) -> Option<Vec<u8>> {
    match rx.recv_timeout(budget) {
        Ok(buf) => Some(buf),
        Err(RecvTimeoutError::Timeout) => None,
        Err(RecvTimeoutError::Disconnected) => Some(Vec::new()),
    }
}

/// Writes an executable shell script and hands back its path, for the
/// behaviours only an actual child process can produce.
///
/// The spawn-and-kill at the end is not pointless. Writing an executable in one
/// test thread while another test's `Command` forks leaves a window where the
/// forked child still holds an inherited write descriptor to the new file, and
/// the kernel refuses to exec a file anyone has open for writing (`ETXTBSY`).
/// That window is absorbed here, where retrying is free, rather than surfacing
/// as a baffling failure in whichever test lost the race.
#[cfg(test)]
pub(crate) fn fake_binary(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fake binary");
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&path).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }

    for _ in 0..200 {
        match Command::new(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_that_finishes_returns_its_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo hello"]);
        let output = output_within(&mut command, Duration::from_secs(5))
            .expect("spawn")
            .expect("a prompt command must not time out");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn a_hanging_command_times_out_promptly() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let start = Instant::now();
        let result = output_within(&mut command, Duration::from_millis(200)).expect("spawn");
        assert!(result.is_none(), "a hanging child must report a timeout");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}; the child was not killed promptly",
            start.elapsed()
        );
    }

    /// The case a plain `join` on the readers turns into an unbounded hang: the
    /// child exits at once, but a descendant still holds the pipes.
    #[test]
    fn a_descendant_holding_the_pipe_does_not_outlast_the_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 6 & exit 0"]);
        let start = Instant::now();
        let result = output_within(&mut command, Duration::from_millis(200)).expect("spawn");
        assert!(
            result.is_none(),
            "an unread pipe past the budget is a timeout"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}; it waited on the orphan's pipe",
            start.elapsed()
        );
    }
}
