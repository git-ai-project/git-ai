use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(200);
const OUTPUT_DRAIN_POLL: Duration = Duration::from_millis(10);
const MAX_TIMED_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;

#[cfg(windows)]
fn timed_command_creation_flags() -> u32 {
    crate::utils::CREATE_NO_WINDOW
}

fn configure_timed_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(timed_command_creation_flags());
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[derive(Debug, Clone)]
pub(crate) struct TimedCommandOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub diagnostics: Vec<String>,
    pub wait_error: Option<String>,
}

enum OutputEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    StdoutDone,
    StderrDone,
    StdoutError(String),
    StderrError(String),
    StdoutTruncated,
    StderrTruncated,
}

#[derive(Default)]
struct OutputState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_done: bool,
    stderr_done: bool,
    stdout_truncated: bool,
    stderr_truncated: bool,
    diagnostics: Vec<String>,
}

impl OutputState {
    fn complete(&self) -> bool {
        self.stdout_done && self.stderr_done
    }

    fn append_output(&mut self, bytes: &[u8], stdout: bool) {
        let (buffer, truncated) = if stdout {
            (&mut self.stdout, &mut self.stdout_truncated)
        } else {
            (&mut self.stderr, &mut self.stderr_truncated)
        };
        let remaining = MAX_TIMED_COMMAND_OUTPUT_BYTES.saturating_sub(buffer.len());
        buffer.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        if bytes.len() > remaining && !*truncated {
            *truncated = true;
            self.diagnostics.push(format!(
                "{} exceeded the {} byte limit; additional output was discarded",
                if stdout { "stdout" } else { "stderr" },
                MAX_TIMED_COMMAND_OUTPUT_BYTES
            ));
        }
    }

    fn mark_truncated(&mut self, stdout: bool) {
        let truncated = if stdout {
            &mut self.stdout_truncated
        } else {
            &mut self.stderr_truncated
        };
        if !*truncated {
            *truncated = true;
            self.diagnostics.push(format!(
                "{} exceeded the {} byte limit; additional output was discarded",
                if stdout { "stdout" } else { "stderr" },
                MAX_TIMED_COMMAND_OUTPUT_BYTES
            ));
        }
    }

    fn finish(
        self,
        status: Option<i32>,
        timed_out: bool,
        wait_error: Option<String>,
    ) -> TimedCommandOutput {
        TimedCommandOutput {
            status,
            stdout: String::from_utf8_lossy(&self.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&self.stderr).trim().to_string(),
            timed_out,
            diagnostics: self.diagnostics,
            wait_error,
        }
    }
}

pub(crate) fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    poll_interval: Duration,
    env_remove: &[&str],
) -> Result<TimedCommandOutput, String> {
    run_command_with_timeout_and_env(program, args, cwd, timeout, poll_interval, env_remove, &[])
}

pub(crate) fn run_command_with_timeout_and_env(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    poll_interval: Duration,
    env_remove: &[&str],
    env_set: &[(&str, &str)],
) -> Result<TimedCommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in env_remove {
        command.env_remove(key);
    }
    for (key, value) in env_set {
        command.env(key, value);
    }
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    configure_timed_command(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to execute: {}", e))?;

    let (tx, rx) = mpsc::channel();
    let mut output = OutputState::default();
    match child.stdout.take() {
        Some(stdout) => spawn_output_reader(stdout, tx.clone(), true),
        None => output.stdout_done = true,
    }
    match child.stderr.take() {
        Some(stderr) => spawn_output_reader(stderr, tx.clone(), false),
        None => output.stderr_done = true,
    }
    drop(tx);

    let start = Instant::now();
    loop {
        drain_output_events(&rx, &mut output);
        match child.try_wait() {
            Ok(Some(status)) => {
                collect_output_until(
                    &rx,
                    &mut output,
                    Instant::now() + OUTPUT_DRAIN_GRACE,
                    OUTPUT_DRAIN_POLL,
                );
                if !output.complete() {
                    output.diagnostics.push(
                        "output collection did not finish after the child exited; descendant processes may still be holding stdout/stderr open".to_string(),
                    );
                }
                return Ok(output.finish(status.code(), false, None));
            }
            Ok(None) if start.elapsed() >= timeout => {
                let kill_result = child.kill();
                match &kill_result {
                    Ok(()) => output
                        .diagnostics
                        .push("sent kill to child process".to_string()),
                    Err(e) => output
                        .diagnostics
                        .push(format!("failed to kill child process: {}", e)),
                }

                let wait_result = child.wait();
                let status = match wait_result {
                    Ok(status) => {
                        output.diagnostics.push(format!(
                            "child process exited after timeout with status {}",
                            status
                                .code()
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| "signal".to_string())
                        ));
                        status.code()
                    }
                    Err(e) => {
                        output
                            .diagnostics
                            .push(format!("failed to wait for child after timeout: {}", e));
                        None
                    }
                };

                collect_output_until(
                    &rx,
                    &mut output,
                    Instant::now() + OUTPUT_DRAIN_GRACE,
                    OUTPUT_DRAIN_POLL,
                );
                if !output.complete() {
                    output.diagnostics.push(
                        "output collection incomplete after timeout; descendant processes may still be holding stdout/stderr open".to_string(),
                    );
                }
                return Ok(output.finish(status, true, None));
            }
            Ok(None) => {
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                collect_output_until(
                    &rx,
                    &mut output,
                    Instant::now() + OUTPUT_DRAIN_GRACE,
                    OUTPUT_DRAIN_POLL,
                );
                return Ok(output.finish(None, false, Some(e.to_string())));
            }
        }
    }
}

fn spawn_output_reader<R>(mut reader: R, tx: Sender<OutputEvent>, stdout: bool)
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        let mut retained = 0usize;
        let mut reported_truncation = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = MAX_TIMED_COMMAND_OUTPUT_BYTES.saturating_sub(retained);
                    let keep = n.min(remaining);
                    if keep > 0 {
                        let event = if stdout {
                            OutputEvent::Stdout(buf[..keep].to_vec())
                        } else {
                            OutputEvent::Stderr(buf[..keep].to_vec())
                        };
                        if tx.send(event).is_err() {
                            return;
                        }
                        retained += keep;
                    }
                    if keep < n && !reported_truncation {
                        reported_truncation = true;
                        let event = if stdout {
                            OutputEvent::StdoutTruncated
                        } else {
                            OutputEvent::StderrTruncated
                        };
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    let event = if stdout {
                        OutputEvent::StdoutError(e.to_string())
                    } else {
                        OutputEvent::StderrError(e.to_string())
                    };
                    let _ = tx.send(event);
                    return;
                }
            }
        }

        let event = if stdout {
            OutputEvent::StdoutDone
        } else {
            OutputEvent::StderrDone
        };
        let _ = tx.send(event);
    });
}

fn collect_output_until(
    rx: &Receiver<OutputEvent>,
    output: &mut OutputState,
    deadline: Instant,
    poll_interval: Duration,
) {
    while !output.complete() && Instant::now() < deadline {
        drain_output_events(rx, output);
        if output.complete() {
            break;
        }
        std::thread::sleep(poll_interval);
    }
    drain_output_events(rx, output);
}

fn drain_output_events(rx: &Receiver<OutputEvent>, output: &mut OutputState) {
    while let Ok(event) = rx.try_recv() {
        match event {
            OutputEvent::Stdout(bytes) => output.append_output(&bytes, true),
            OutputEvent::Stderr(bytes) => output.append_output(&bytes, false),
            OutputEvent::StdoutDone => output.stdout_done = true,
            OutputEvent::StderrDone => output.stderr_done = true,
            OutputEvent::StdoutError(err) => {
                output
                    .diagnostics
                    .push(format!("failed to read stdout: {}", err));
                output.stdout_done = true;
            }
            OutputEvent::StderrError(err) => {
                output
                    .diagnostics
                    .push(format!("failed to read stderr: {}", err));
                output.stderr_done = true;
            }
            OutputEvent::StdoutTruncated => output.mark_truncated(true),
            OutputEvent::StderrTruncated => output.mark_truncated(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_state_rejects_subprocess_output_beyond_limit() {
        let (tx, rx) = mpsc::channel();
        tx.send(OutputEvent::Stdout(vec![b'x'; 1024 * 1024 + 1]))
            .unwrap();
        drop(tx);
        let mut output = OutputState::default();

        drain_output_events(&rx, &mut output);

        assert!(output.stdout.len() <= 1024 * 1024);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|message| message.contains("byte limit"))
        );
    }

    #[test]
    fn output_reader_stops_queueing_after_limit() {
        let input = std::io::Cursor::new(vec![b'x'; MAX_TIMED_COMMAND_OUTPUT_BYTES + 8192]);
        let (tx, rx) = mpsc::channel();
        spawn_output_reader(input, tx, true);

        let mut queued_bytes = 0usize;
        let mut truncated = false;
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(1)) {
            match event {
                OutputEvent::Stdout(bytes) => queued_bytes += bytes.len(),
                OutputEvent::StdoutTruncated => truncated = true,
                OutputEvent::StdoutDone => break,
                _ => {}
            }
        }

        assert_eq!(queued_bytes, MAX_TIMED_COMMAND_OUTPUT_BYTES);
        assert!(truncated);
    }

    #[cfg(windows)]
    #[test]
    fn timed_commands_do_not_create_console_windows() {
        assert_eq!(
            timed_command_creation_flags(),
            crate::utils::CREATE_NO_WINDOW
        );
    }
}
