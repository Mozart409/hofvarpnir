//! The `DownloadWorker` is a short-lived actor spawned per video download.
//! It shells out to `yt-dlp` with the appropriate profile arguments, reads
//! structured progress from stdout via `--progress-template`, and reports
//! progress back to the `DownloadSupervisor`.
//!
//! Uses `kill_on_drop(true)` to prevent orphaned yt-dlp processes and
//! `tokio::time::timeout` to enforce a max download duration.
