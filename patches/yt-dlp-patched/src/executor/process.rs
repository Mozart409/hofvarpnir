//! Process execution and output handling.

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{Error, Result};

/// Represents the output of a process.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessOutput {
    /// The stdout of the process.
    pub stdout: String,
    /// The stderr of the process.
    pub stderr: String,
    /// The exit code of the process.
    pub code: i32,
}

impl std::fmt::Display for ProcessOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ProcessOutput(code={}, stdout_len={}, stderr_len={})",
            self.code,
            self.stdout.len(),
            self.stderr.len()
        )
    }
}

/// Executes a command with the given arguments and timeout.
///
/// # Arguments
///
/// * `executable_path` - Path to the executable
/// * `args` - Arguments to pass to the command
/// * `timeout` - Maximum duration to wait for the process
///
/// # Errors
///
/// Returns an error if the command fails, times out, or cannot be executed
pub async fn execute_command(
    executable_path: impl Into<PathBuf>,
    args: &[String],
    timeout: Duration,
) -> Result<ProcessOutput> {
    execute_command_internal(executable_path, args, timeout, None).await
}

/// Executes a command and redirects stdout to a file.
///
/// # Arguments
///
/// * `executable_path` - Path to the executable
/// * `args` - Arguments to pass to the command
/// * `timeout` - Maximum duration to wait for the process
/// * `output_path` - Path to the file where stdout will be written
///
/// # Errors
///
/// Returns an error if the command fails, times out, or cannot be executed
pub async fn execute_command_to_file(
    executable_path: impl Into<PathBuf>,
    args: &[String],
    timeout: Duration,
    output_path: impl Into<PathBuf>,
) -> Result<ProcessOutput> {
    execute_command_internal(executable_path, args, timeout, Some(output_path.into())).await
}

/// Internal command execution with optional file output
///
/// # Arguments
///
/// * `executable_path` - Path to the executable
/// * `args` - Arguments to pass to the command
/// * `timeout` - Maximum duration to wait for the process
/// * `output_path` - Optional path to redirect stdout to a file
///
/// # Returns
///
/// ProcessOutput containing stdout (if not redirected), stderr, and exit code
///
/// # Errors
///
/// Returns an error if the command fails, times out, or cannot be executed
// LCOV_EXCL_START — requires real yt-dlp/ffmpeg binary on PATH
#[tracing::instrument(
    name = "process",
    skip_all,
    fields(
        executable = tracing::field::Empty,
        args = ?args,
        timeout_secs = timeout.as_secs(),
        pid = tracing::field::Empty,
        exit_code = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
        timed_out = false,
        stderr_tail = tracing::field::Empty,
    )
)]
async fn execute_command_internal(
    executable_path: impl Into<PathBuf>,
    args: &[String],
    timeout: Duration,
    output_path: Option<PathBuf>,
) -> Result<ProcessOutput> {
    let executable_path: PathBuf = executable_path.into();
    let span = tracing::Span::current();
    span.record("executable", tracing::field::display(executable_path.display()));
    let started = std::time::Instant::now();

    tracing::debug!(
        executable = ?executable_path,
        arg_count = args.len(),
        timeout_secs = timeout.as_secs(),
        output_to_file = output_path.is_some(),
        output_path = ?output_path,
        "⚙️ Starting command execution"
    );

    let mut command = tokio::process::Command::new(&executable_path);

    // Configure stdout: either pipe (memory) or file
    if let Some(path) = &output_path {
        let file = std::fs::File::create(path)?;
        command.stdout(std::process::Stdio::from(file));
    } else {
        command.stdout(std::process::Stdio::piped());
    }

    command.stderr(std::process::Stdio::piped());

    // If the caller drops this future (an outer download timeout, a cancelled
    // task), kill the child too. Otherwise FFmpeg keeps running detached and
    // finishes writing a temp or output file whose cleanup code never runs.
    command.kill_on_drop(true);

    #[cfg(target_os = "windows")]
    command.creation_flags(0x08000000);

    command.args(args);

    tracing::debug!(
        executable = ?executable_path,
        "⚙️ Spawning child process"
    );

    let mut child = command.spawn()?;
    if let Some(pid) = child.id() {
        span.record("pid", pid);
    }

    tracing::debug!(
        executable = ?executable_path,
        pid = ?child.id(),
        "✅ Child process spawned"
    );

    // Read streams asynchronously
    let stdout_task = if output_path.is_none() {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::io("capture stdout", std::io::Error::other("stdout stream not available")))?;

        Some(tokio::spawn(read_stream(stdout)))
    } else {
        None
    };

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::io("capture stderr", std::io::Error::other("stderr stream not available")))?;

    let stderr_task = tokio::spawn(read_stream(stderr));

    tracing::debug!(
        executable = ?executable_path,
        timeout_secs = timeout.as_secs(),
        "⚙️ Waiting for process to complete"
    );

    // Wait for the process to finish with timeout
    let exit_status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result?,
        Err(_) => {
            span.record("timed_out", true);
            span.record("duration_ms", elapsed_ms(started));
            tracing::warn!(
                executable = ?executable_path,
                timeout_secs = timeout.as_secs(),
                "⚙️ Process timed out, killing it"
            );

            if let Err(e) = child.kill().await {
                tracing::error!(
                    executable = ?executable_path,
                    error = %e,
                    "⚙️ Failed to kill process after timeout"
                );
            } else if let Err(e) = child.wait().await {
                tracing::error!(
                    executable = ?executable_path,
                    error = %e,
                    "⚙️ Failed to wait for process after kill"
                );
            }

            return Err(Error::Timeout {
                operation: format!("executing command: {}", executable_path.display()),
                duration: timeout,
            });
        }
    };

    tracing::debug!(
        executable = ?executable_path,
        exit_code = exit_status.code().unwrap_or(-1),
        success = exit_status.success(),
        "⚙️ Process completed"
    );

    // Read stderr stream
    let stderr_result = match stderr_task.await {
        Ok(Ok(buffer)) => buffer,
        Ok(Err(e)) => return Err(Error::io("reading command stderr", e)),
        Err(e) => return Err(Error::runtime("reading command stderr task", e)),
    };

    let stdout_result = if let Some(task) = stdout_task {
        match task.await {
            Ok(Ok(buffer)) => buffer,
            Ok(Err(e)) => return Err(Error::io("reading command stdout", e)),
            Err(e) => return Err(Error::runtime("reading command stdout task", e)),
        }
    } else {
        Vec::new()
    };

    // Convert the buffers to Strings (lossy to avoid errors on non-UTF8 output)
    let stdout = String::from_utf8_lossy(&stdout_result).to_string();
    let stderr = String::from_utf8_lossy(&stderr_result).to_string();
    let code = exit_status.code().unwrap_or(-1);
    let stderr_tail = tail_chars(&stderr, STDERR_TAIL_CHARS);

    span.record("exit_code", code);
    span.record("duration_ms", elapsed_ms(started));
    if !stderr_tail.is_empty() {
        span.record("stderr_tail", stderr_tail);
    }

    tracing::debug!(
        executable = ?executable_path,
        exit_code = code,
        stdout_len = stdout.len(),
        stderr_len = stderr.len(),
        stderr_tail,
        "⚙️ Command output captured"
    );

    // FFmpeg exits 0 when it declines to overwrite an existing output file,
    // having written nothing. Treating that as success publishes whatever was
    // already at the output path -- measured in production as a killed,
    // moov-less MP4 left behind by an earlier timed-out mux.
    if exit_status.success() && ffmpeg_refused_overwrite(&stderr) {
        tracing::warn!(
            executable = ?executable_path,
            stderr_tail,
            "⚙️ FFmpeg refused to overwrite existing output and wrote nothing"
        );

        return Err(Error::CommandFailed {
            command: executable_path.display().to_string(),
            exit_code: code,
            stderr,
        });
    }

    if exit_status.success() {
        tracing::debug!(
            executable = ?executable_path,
            exit_code = code,
            "✅ Command execution succeeded"
        );

        return Ok(ProcessOutput { stdout, stderr, code });
    }

    tracing::warn!(
        executable = ?executable_path,
        exit_code = code,
        stderr_tail,
        "⚙️ Command execution failed"
    );

    Err(Error::CommandFailed {
        command: executable_path.display().to_string(),
        exit_code: code,
        stderr,
    })
}
// LCOV_EXCL_STOP

/// How much of a child's stderr to keep on its span and in failure logs.
///
/// The end of stderr is where FFmpeg and yt-dlp put the reason they stopped.
const STDERR_TAIL_CHARS: usize = 2000;

/// Returns at most the last `max` characters of `s`, never splitting a char.
fn tail_chars(s: &str, max: usize) -> &str {
    let s = s.trim_end();
    if max == 0 {
        return "";
    }
    s.char_indices().rev().nth(max - 1).map_or(s, |(idx, _)| &s[idx..])
}

/// Whether FFmpeg's stderr says it skipped writing because the output existed.
///
/// Covers both forms: the interactive prompt answered by a closed stdin, and
/// the `-nostdin` / `-n` form.
fn ffmpeg_refused_overwrite(stderr: &str) -> bool {
    stderr.contains("Not overwriting - exiting") || stderr.contains("already exists. Exiting.")
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Helper function to read a stream into a buffer
///
/// # Arguments
///
/// * `stream` - An async readable stream (stdout or stderr)
///
/// # Returns
///
/// A vector of bytes containing all data read from the stream
///
/// # Errors
///
/// Returns an IO error if reading fails
async fn read_stream<R>(mut stream: R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = Vec::new();
    let bytes_read = tokio::io::copy(&mut tokio::io::BufReader::new(&mut stream), &mut buffer).await?;

    tracing::trace!(bytes_read, "Stream read completed");
    Ok(buffer)
}
