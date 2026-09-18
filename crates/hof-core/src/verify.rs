//! Post-download media verification.
//!
//! A download that exits cleanly is not necessarily a playable file. The
//! segmented downloader pre-allocates the output with `set_len` before writing
//! (`parallel.rs`), so a lost segment leaves a full-size file with a
//! zero-filled hole in it — the file size matches expectations exactly and the
//! only symptom is a player failing at watch time, typically with
//! `moov atom not found` when the hole lands in an MP4's index.
//!
//! This module gates that: the file is probed and demuxed *before* it is moved
//! out of `incomplete/`, so a damaged download fails as a download and picks up
//! the supervisor's existing retry/backoff instead of reaching the library.
//!
//! # Why demux instead of decode
//!
//! Verification runs `ffmpeg -i <file> -c copy -f null -`, which reads every
//! byte and validates container framing without decoding any frames. That
//! matters for two reasons:
//!
//! - **Coverage.** Segments are written out of order, so damage is not biased
//!   toward the end of the file. Sampling a window (e.g. `-sseof -30`) inspects
//!   a fraction of a long recording and misses the rest. Worse, on a file with
//!   a front-loaded index (see `+faststart` below) `-sseof` seeks past EOF,
//!   decodes zero frames, and *succeeds* — a silent pass on a broken file.
//! - **Cost.** Demuxing is I/O-bound rather than codec-bound, so a 1440p60 AV1
//!   recording costs the same per gigabyte as anything else. Measured at
//!   roughly 0.7s per 500MB, versus ~26s for a full decode of the same file.
//!
//! Note that `ffmpeg` exits 0 even when it reports stream errors, so the demux
//! pass is judged on whether it wrote anything to stderr at `-v error`, not on
//! its exit status.
//!
//! # Why the demux pass is not enough on its own
//!
//! Demuxing catches *truncation* in every codec, because the sample table
//! promises bytes that are not there. It does not reliably catch an *interior*
//! hole: only some codecs carry framing the demuxer validates in-band. Measured
//! against a file with a zeroed region in the middle, `-c copy` reports
//! `Invalid NAL unit size` for H.264 but passes clean for AV1 and HEVC — and
//! AV1 is what the `Browser`/`Tv` ladders actually deliver above 1080p.
//!
//! So a zero-run scan runs alongside it. It targets the specific defect rather
//! than the codec: a dropped segment leaves the pre-allocated region untouched,
//! i.e. a multi-megabyte run of `0x00`. Segment sizes start at 5 MiB
//! (`speed_profile.rs`), so any real hole is far larger than
//! [`ZERO_RUN_THRESHOLD`], while compressed media never contains a run that
//! long legitimately.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tracing::{debug, instrument, warn};

/// Upper bound on a single verification pass.
///
/// Demuxing is disk-bound; this exists to stop a wedged `ffmpeg` from pinning a
/// worker for the rest of the process's life, not to bound normal work.
const VERIFY_TIMEOUT: Duration = Duration::from_mins(15);

/// Fraction of the expected duration the container may fall short by.
const DURATION_TOLERANCE_RATIO: f64 = 0.02;

/// Floor for the duration tolerance, in seconds.
///
/// Extractor-reported and container durations routinely disagree by a second or
/// two on an intact file; without a floor, short videos would false-positive.
const DURATION_TOLERANCE_MIN_SECS: f64 = 5.0;

/// Length of an all-zero run that is treated as a dropped download segment.
///
/// Sits below the smallest segment size the downloader uses (5 MiB) so any real
/// hole trips it, and far above any padding a muxer would legitimately emit.
const ZERO_RUN_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Read buffer for the zero-run scan.
const SCAN_CHUNK_BYTES: usize = 1024 * 1024;

/// Reasons a downloaded file can fail verification.
#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    /// The verification tool could not be run at all.
    #[error("{binary} could not be run: {detail}")]
    ToolUnavailable {
        binary: &'static str,
        detail: String,
    },

    /// Verification exceeded [`VERIFY_TIMEOUT`].
    #[error("verification timed out after {0:?}")]
    TimedOut(Duration),

    /// `ffprobe` could not read the container at all.
    ///
    /// This is the `moov atom not found` case for files whose index sits at the
    /// end and was never written.
    #[error("container is unreadable: {0}")]
    UnreadableContainer(String),

    /// The container parsed but reports no usable duration.
    #[error("container reports no usable duration (got {0:?})")]
    NoDuration(Option<String>),

    /// An expected stream is absent, e.g. a mux that died leaving video only.
    #[error("no {0} stream present")]
    MissingStream(&'static str),

    /// The container is well-formed but shorter than the extractor reported.
    #[error("container duration {actual:.0}s is short of the expected {expected}s")]
    DurationMismatch { actual: f64, expected: i64 },

    /// The demux pass reported framing or stream errors.
    #[error("stream data is damaged: {0}")]
    DamagedStreams(String),

    /// A run of zero bytes long enough to be a dropped download segment.
    #[error("found a {run_bytes}-byte run of zeros at offset {offset} (dropped segment)")]
    ZeroRun { offset: u64, run_bytes: u64 },

    /// The file could not be read for scanning.
    #[error("could not read file: {0}")]
    Unreadable(String),
}

/// What the downloaded file is expected to contain.
#[derive(Debug, Clone)]
pub struct Expectations {
    /// Whether a video stream must be present. False for audio-only downloads.
    pub expect_video: bool,
    /// Whether an audio stream must be present.
    pub expect_audio: bool,
    /// Duration the extractor reported, when known.
    pub duration_secs: Option<i64>,
}

/// Streams and duration as reported by `ffprobe`.
#[derive(Debug, Default)]
struct Probe {
    duration: Option<f64>,
    has_video: bool,
    has_audio: bool,
}

/// Verify that a downloaded file is intact and matches `expect`.
///
/// Runs a cheap `ffprobe` pass (container readable, expected streams present,
/// duration plausible) followed by a full demux pass that reads every byte.
///
/// # Errors
///
/// Returns a [`VerificationError`] describing the first failed check. A
/// successful return means the container parsed, the expected streams are
/// present, the duration is consistent with `expect`, and no stream errors were
/// reported while reading the file end to end.
#[instrument(skip_all, fields(path = %path.display()))]
pub async fn verify_media(path: &Path, expect: &Expectations) -> Result<(), VerificationError> {
    let probe = run_probe(path).await?;

    let Some(duration) = probe.duration.filter(|d| *d > 0.0) else {
        return Err(VerificationError::NoDuration(
            probe.duration.map(|d| d.to_string()),
        ));
    };

    if expect.expect_video && !probe.has_video {
        return Err(VerificationError::MissingStream("video"));
    }
    if expect.expect_audio && !probe.has_audio {
        return Err(VerificationError::MissingStream("audio"));
    }

    // Only a *short* container signals a truncated download. Containers
    // routinely run slightly long (trailing audio padding), which is harmless.
    if let Some(expected) = expect.duration_secs.filter(|e| *e > 0) {
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let expected_f = expected as f64;
        let tolerance = (expected_f * DURATION_TOLERANCE_RATIO).max(DURATION_TOLERANCE_MIN_SECS);
        if duration < expected_f - tolerance {
            return Err(VerificationError::DurationMismatch {
                actual: duration,
                expected,
            });
        }
    }

    // Ordered cheapest-first among the full-file passes: the scan is a plain
    // sequential read, the demux spawns a process and parses containers.
    scan_for_zero_runs(path).await?;
    run_demux(path).await?;

    debug!(duration, "Media verified");
    Ok(())
}

/// Read duration and stream types via `ffprobe`.
async fn run_probe(path: &Path) -> Result<Probe, VerificationError> {
    let mut command = Command::new("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-hide_banner",
            "-show_entries",
            "format=duration:stream=codec_type",
            "-of",
            "default=nw=1",
            "--",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let output = run_tool(command, "ffprobe").await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VerificationError::UnreadableContainer(first_line(
            stderr.trim(),
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut probe = Probe::default();
    for line in stdout.lines() {
        match line.trim() {
            "codec_type=video" => probe.has_video = true,
            "codec_type=audio" => probe.has_audio = true,
            other => {
                if let Some(value) = other.strip_prefix("duration=")
                    && let Ok(parsed) = value.parse::<f64>()
                {
                    probe.duration = Some(parsed);
                }
            }
        }
    }

    Ok(probe)
}

/// Read the whole file through `ffmpeg` without decoding, checking framing.
async fn run_demux(path: &Path) -> Result<(), VerificationError> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-xerror", "-i"])
        .arg(path)
        .args(["-c", "copy", "-f", "null", "-"])
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let output = run_tool(command, "ffmpeg").await?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();

    // ffmpeg reports stream errors on stderr while still exiting 0, so the
    // presence of output is the signal here, not the exit status.
    if !stderr.is_empty() {
        warn!(detail = %stderr, "Demux pass reported stream errors");
        return Err(VerificationError::DamagedStreams(first_line(stderr)));
    }

    Ok(())
}

/// Scan the file for an all-zero run long enough to be a dropped segment.
///
/// Codec-independent, and the only check that reliably catches an interior hole
/// in AV1 or HEVC (see the module docs).
async fn scan_for_zero_runs(path: &Path) -> Result<(), VerificationError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| VerificationError::Unreadable(e.to_string()))?;

    let mut buffer = vec![0u8; SCAN_CHUNK_BYTES];
    let mut offset: u64 = 0;
    // Carried across chunk boundaries so a hole spanning chunks still counts.
    let mut run_bytes: u64 = 0;
    let mut run_start: u64 = 0;

    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| VerificationError::Unreadable(e.to_string()))?;
        if read == 0 {
            break;
        }

        let Some(chunk) = buffer.get(..read) else {
            break;
        };

        for (index, byte) in chunk.iter().enumerate() {
            if *byte == 0 {
                if run_bytes == 0 {
                    run_start = offset.saturating_add(u64::try_from(index).unwrap_or(u64::MAX));
                }
                run_bytes += 1;
                if run_bytes >= ZERO_RUN_THRESHOLD {
                    return Err(VerificationError::ZeroRun {
                        offset: run_start,
                        run_bytes,
                    });
                }
            } else {
                run_bytes = 0;
            }
        }

        offset = offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }

    Ok(())
}

/// Run one verification tool under [`VERIFY_TIMEOUT`].
async fn run_tool(
    mut command: Command,
    binary: &'static str,
) -> Result<std::process::Output, VerificationError> {
    match tokio::time::timeout(VERIFY_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(VerificationError::ToolUnavailable {
            binary,
            detail: e.to_string(),
        }),
        Err(_) => Err(VerificationError::TimedOut(VERIFY_TIMEOUT)),
    }
}

/// Collapse multi-line tool output to its first line for error messages.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expectations(duration_secs: Option<i64>) -> Expectations {
        Expectations {
            expect_video: true,
            expect_audio: true,
            duration_secs,
        }
    }

    #[tokio::test]
    async fn missing_file_is_unreadable() {
        let err = verify_media(Path::new("/nonexistent/video.mp4"), &expectations(None))
            .await
            .unwrap_err();
        assert!(matches!(err, VerificationError::UnreadableContainer(_)));
    }

    #[tokio::test]
    async fn empty_file_is_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("empty.mp4");
        std::fs::write(&path, b"").unwrap();

        let err = verify_media(&path, &expectations(None)).await.unwrap_err();
        assert!(matches!(err, VerificationError::UnreadableContainer(_)));
    }

    #[tokio::test]
    async fn garbage_file_is_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("garbage.mp4");
        std::fs::write(&path, vec![0u8; 64 * 1024]).unwrap();

        let err = verify_media(&path, &expectations(None)).await.unwrap_err();
        assert!(matches!(err, VerificationError::UnreadableContainer(_)));
    }

    #[test]
    fn duration_tolerance_floor_applies_to_short_videos() {
        // A 10s video may not use the 2% ratio (0.2s) as its tolerance.
        let expected = 10.0_f64;
        let tolerance = (expected * DURATION_TOLERANCE_RATIO).max(DURATION_TOLERANCE_MIN_SECS);
        assert!((tolerance - DURATION_TOLERANCE_MIN_SECS).abs() < f64::EPSILON);
    }

    #[test]
    fn duration_tolerance_ratio_applies_to_long_videos() {
        // A 2h talk gets 2% (144s), not the floor.
        let expected = 7200.0_f64;
        let tolerance = (expected * DURATION_TOLERANCE_RATIO).max(DURATION_TOLERANCE_MIN_SECS);
        assert!((tolerance - 144.0).abs() < f64::EPSILON);
    }

    /// Generate a small test clip with ffmpeg.
    ///
    /// Uses `mpeg4`/`aac`, which are built into every ffmpeg, so these tests do
    /// not depend on which external encoders the build was linked against.
    /// ffmpeg itself is already a hard startup requirement (`verify_ffmpeg_binary`).
    fn make_clip(path: &Path, secs: u32, with_audio: bool) {
        let mut command = std::process::Command::new("ffmpeg");
        command.args(["-v", "error", "-nostdin", "-f", "lavfi", "-i"]);
        command.arg(format!("testsrc=d={secs}:s=160x120:r=15"));
        if with_audio {
            command
                .args(["-f", "lavfi", "-i"])
                .arg(format!("sine=d={secs}"));
        }
        command.args(["-c:v", "mpeg4"]);
        if with_audio {
            command.args(["-c:a", "aac"]);
        }
        let status = command
            .args(["-movflags", "+faststart", "-y"])
            .arg(path)
            .status()
            .expect("ffmpeg must be available to run verification tests");
        assert!(status.success(), "ffmpeg failed to generate {path:?}");
    }

    #[tokio::test]
    async fn intact_clip_verifies() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.mp4");
        make_clip(&path, 3, true);

        verify_media(&path, &expectations(Some(3))).await.unwrap();
    }

    #[tokio::test]
    async fn truncated_clip_is_rejected() {
        // The production symptom: a file cut short mid-write. With +faststart
        // the index survives and ffprobe still reports the full duration, so
        // this must be caught by the demux pass, not the probe.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.mp4");
        make_clip(&path, 3, true);

        let full_len = std::fs::metadata(&path).unwrap().len();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(full_len * 7 / 10).unwrap();
        drop(file);

        let err = verify_media(&path, &expectations(Some(3)))
            .await
            .unwrap_err();
        assert!(
            matches!(err, VerificationError::DamagedStreams(_)),
            "expected DamagedStreams, got {err:?}"
        );
    }

    #[tokio::test]
    async fn video_only_clip_fails_the_audio_expectation() {
        // A mux that dies after the video stream leaves a playable but silent file.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("silent.mp4");
        make_clip(&path, 2, false);

        let err = verify_media(&path, &expectations(None)).await.unwrap_err();
        assert!(
            matches!(err, VerificationError::MissingStream("audio")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn audio_only_expectation_ignores_the_video_stream() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("silent.mp4");
        make_clip(&path, 2, false);

        let expect = Expectations {
            expect_video: false,
            expect_audio: false,
            duration_secs: None,
        };
        verify_media(&path, &expect).await.unwrap();
    }

    #[tokio::test]
    async fn clip_shorter_than_expected_is_rejected() {
        // A download that stopped early but still muxed a well-formed container.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.mp4");
        make_clip(&path, 3, true);

        let err = verify_media(&path, &expectations(Some(600)))
            .await
            .unwrap_err();
        match err {
            VerificationError::DurationMismatch { expected, .. } => assert_eq!(expected, 600),
            other => panic!("expected DurationMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duration_within_tolerance_is_accepted() {
        // Extractor and container durations disagree slightly on intact files.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.mp4");
        make_clip(&path, 3, true);

        verify_media(&path, &expectations(Some(6))).await.unwrap();
    }

    /// [`ZERO_RUN_THRESHOLD`] as a buffer length.
    fn threshold() -> usize {
        usize::try_from(ZERO_RUN_THRESHOLD).unwrap()
    }

    /// Write `bytes` to a temp file and scan it.
    async fn scan_bytes(bytes: &[u8]) -> Result<(), VerificationError> {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("scan.bin");
        std::fs::write(&path, bytes).unwrap();
        scan_for_zero_runs(&path).await
    }

    #[tokio::test]
    async fn dense_data_passes_the_zero_scan() {
        // Compressed media is dense; no long zero runs.
        let data: Vec<u8> = (1u8..=255).cycle().take(8 * 1024 * 1024).collect();
        assert!(scan_bytes(&data).await.is_ok());
    }

    #[tokio::test]
    async fn short_zero_runs_pass_the_zero_scan() {
        // Muxer padding and incidental zeros must not trip the scan.
        let mut data = vec![1u8; 6 * 1024 * 1024];
        let gap = threshold() - 1;
        data.splice(1024..1024 + gap, std::iter::repeat_n(0u8, gap));
        assert!(scan_bytes(&data).await.is_ok());
    }

    #[tokio::test]
    async fn dropped_segment_sized_zero_run_is_rejected() {
        // A lost 5 MiB segment leaves the pre-allocated region untouched.
        let mut data = vec![7u8; 12 * 1024 * 1024];
        let hole = 5 * 1024 * 1024;
        data.splice(2_000_000..2_000_000 + hole, std::iter::repeat_n(0u8, hole));

        let err = scan_bytes(&data).await.unwrap_err();
        match err {
            VerificationError::ZeroRun { offset, run_bytes } => {
                assert_eq!(offset, 2_000_000);
                assert_eq!(run_bytes, ZERO_RUN_THRESHOLD);
            }
            other => panic!("expected ZeroRun, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn zero_run_spanning_chunk_boundaries_is_rejected() {
        // The run must be carried across SCAN_CHUNK_BYTES reads. Start the hole
        // just before a chunk edge so no single chunk contains the threshold.
        let start = SCAN_CHUNK_BYTES - 16;
        let hole = threshold();
        let mut data = vec![3u8; start + hole + 4096];
        data.splice(start..start + hole, std::iter::repeat_n(0u8, hole));

        let err = scan_bytes(&data).await.unwrap_err();
        assert!(
            matches!(
                err,
                VerificationError::ZeroRun { offset, .. }
                    if offset == u64::try_from(start).unwrap()
            ),
            "expected ZeroRun at {start}, got {err:?}"
        );
    }

    #[tokio::test]
    async fn trailing_zero_run_at_eof_is_rejected() {
        let mut data = vec![9u8; 1024];
        data.extend(std::iter::repeat_n(0u8, threshold()));
        assert!(matches!(
            scan_bytes(&data).await.unwrap_err(),
            VerificationError::ZeroRun { .. }
        ));
    }

    #[test]
    fn first_line_collapses_multiline_output() {
        assert_eq!(first_line("one\ntwo\nthree"), "one");
        assert_eq!(first_line("  only  "), "only");
        assert_eq!(first_line(""), "");
    }
}
