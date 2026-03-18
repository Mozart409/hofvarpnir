//! The `DownloadWorker` is a short-lived actor spawned per video download.
//!
//! It shells out to `yt-dlp` with the appropriate profile arguments, reads
//! structured progress from stdout via `--progress-template`, and reports
//! progress back to the `DownloadSupervisor`.
//!
//! Uses `kill_on_drop(true)` to prevent orphaned yt-dlp processes and
//! `tokio::time::timeout` to enforce a max download duration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Datelike;
use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::db;
use crate::domain::profile::Quality;
use crate::domain::video::{DownloadProgress, Video};
use crate::ytdlp::{DownloadRequest, DownloadResult, OutputTemplateData, YtdlpClient, YtdlpError};

const INCOMPLETE_DIR_NAME: &str = "incomplete";
const COMPLETED_DIR_NAME: &str = "completed";

/// Configuration for a download worker.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Maximum time allowed for a single download.
    pub timeout: Duration,
    /// Quality setting for the download.
    pub quality: Quality,
    /// Output directory for the downloaded file.
    pub output_dir: PathBuf,
    /// Naming template for the output file.
    pub naming_template: String,
    /// Source id for template context.
    pub source_id: Ulid,
    /// Source display name for template context.
    pub source_name: String,
}

/// Result of a download operation, sent back to the supervisor.
#[derive(Debug, Clone, Reply)]
pub enum DownloadOutcome {
    /// Download completed successfully.
    Success {
        video_id: Ulid,
        file_path: PathBuf,
        file_size_bytes: i64,
    },
    /// Download failed with an error.
    Failed {
        video_id: Ulid,
        error: String,
        is_rate_limited: bool,
    },
}

/// The download worker actor.
///
/// This actor is short-lived: it processes a single download and then stops.
/// Progress updates are sent via a channel to the supervisor.
pub struct DownloadWorker {
    /// Database pool for status updates.
    pool: PgPool,
    /// The video to download.
    video: Video,
    /// Download configuration.
    config: DownloadConfig,
    /// yt-dlp client.
    ytdlp: Arc<YtdlpClient>,
    /// Channel to send progress updates.
    progress_tx: mpsc::Sender<DownloadProgress>,
}

impl std::fmt::Debug for DownloadWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadWorker")
            .field("video_id", &self.video.id)
            .field("platform", &self.video.platform)
            .field("platform_video_id", &self.video.platform_video_id)
            .finish_non_exhaustive()
    }
}

/// Arguments needed to spawn a `DownloadWorker`.
pub struct DownloadWorkerArgs {
    pub pool: PgPool,
    pub video: Video,
    pub config: DownloadConfig,
    pub ytdlp: Arc<YtdlpClient>,
    pub progress_tx: mpsc::Sender<DownloadProgress>,
}

impl Actor for DownloadWorker {
    type Args = DownloadWorkerArgs;
    type Error = color_eyre::eyre::Error;

    #[instrument(skip_all, fields(video_id = %args.video.id))]
    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        info!(
            video_id = %args.video.id,
            title = %args.video.title,
            "Download worker starting"
        );

        let worker = Self {
            pool: args.pool,
            video: args.video,
            config: args.config,
            ytdlp: args.ytdlp,
            progress_tx: args.progress_tx,
        };

        // Immediately trigger the download.
        // Use try_send() to avoid potential deadlock from self-tell with bounded mailbox.
        if let Err(e) = actor_ref.tell(StartDownload).try_send() {
            error!(error = %e, "Failed to start download");
            return Err(e.into());
        }

        Ok(worker)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        debug!(
            video_id = %self.video.id,
            reason = ?reason,
            "Download worker stopping"
        );
        Ok(())
    }
}

/// Message to start the download process.
pub struct StartDownload;

impl Message<StartDownload> for DownloadWorker {
    type Reply = DownloadOutcome;

    #[instrument(skip_all, fields(video_id = %self.video.id))]
    async fn handle(
        &mut self,
        _msg: StartDownload,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let outcome = self.execute_download().await;

        // Stop the actor after the download completes
        ctx.actor_ref().stop_gracefully().await.ok();

        outcome
    }
}

impl DownloadWorker {
    /// Execute the actual download with timeout.
    #[instrument(skip(self), fields(video_id = %self.video.id, title = %self.video.title))]
    async fn execute_download(&mut self) -> DownloadOutcome {
        let video_id = self.video.id;

        // Mark video as downloading in the database
        if let Err(e) = db::mark_video_downloading(&self.pool, video_id).await {
            error!(error = %e, "Failed to mark video as downloading");
            return DownloadOutcome::Failed {
                video_id,
                error: format!("Database error: {e}"),
                is_rate_limited: false,
            };
        }

        info!("Starting download");

        // Build the video URL
        let url = self.build_video_url();
        let template_data = self.build_template_data().await;

        // Execute download with timeout
        let incomplete_dir = self.incomplete_dir();
        let download_future = self.ytdlp.download_video_to_dir(DownloadRequest {
            url: &url,
            output_dir: &incomplete_dir,
            naming_template: &self.config.naming_template,
            template_data: &template_data,
            quality: &self.config.quality,
            video_id,
            progress_tx: Some(self.progress_tx.clone()),
        });

        let result = tokio::time::timeout(self.config.timeout, download_future).await;

        match result {
            Ok(Ok(download_result)) => self.handle_success(download_result).await,
            Ok(Err(ref e)) => self.handle_error(e),
            Err(_) => {
                let error = format!("Download timed out after {:?}", self.config.timeout);
                warn!(error = %error, "Download timed out");
                DownloadOutcome::Failed {
                    video_id,
                    error,
                    is_rate_limited: false,
                }
            }
        }
    }

    /// Build the video URL from platform and video ID.
    fn build_video_url(&self) -> String {
        match self.video.platform.as_str() {
            "youtube" => format!(
                "https://www.youtube.com/watch?v={}",
                self.video.platform_video_id
            ),
            "vimeo" => format!("https://vimeo.com/{}", self.video.platform_video_id),
            "twitter" | "x" => format!(
                "https://twitter.com/i/status/{}",
                self.video.platform_video_id
            ),
            "tiktok" => format!(
                "https://www.tiktok.com/@unknown/video/{}",
                self.video.platform_video_id
            ),
            "twitch" => format!(
                "https://www.twitch.tv/videos/{}",
                self.video.platform_video_id
            ),
            _ => {
                // For unknown platforms, try to use platform_video_id as full URL
                // if it looks like a URL, otherwise construct a generic one
                if self.video.platform_video_id.starts_with("http") {
                    self.video.platform_video_id.clone()
                } else {
                    format!(
                        "https://{}.com/{}",
                        self.video.platform, self.video.platform_video_id
                    )
                }
            }
        }
    }

    /// Handle a successful download.
    async fn handle_success(&self, result: DownloadResult) -> DownloadOutcome {
        let video_id = self.video.id;
        let incomplete_dir = self.incomplete_dir();
        let completed_dir = self.completed_dir();
        let final_path = match Self::move_to_completed(
            &result.file_path,
            &incomplete_dir,
            &completed_dir,
        )
        .await
        {
            Ok(path) => path,
            Err(error) => {
                let message = format!(
                    "Downloaded media but failed to move file into completed directory: {error}"
                );
                error!(video_id = %video_id, error = %message, "Failed to finalize completed file");
                return DownloadOutcome::Failed {
                    video_id,
                    error: message,
                    is_rate_limited: false,
                };
            }
        };
        let file_path_str = final_path.to_string_lossy().to_string();

        info!(
            file_path = %file_path_str,
            file_size = result.file_size_bytes,
            "Download completed successfully"
        );

        // Update database
        if let Err(e) =
            db::mark_video_completed(&self.pool, video_id, &file_path_str, result.file_size_bytes)
                .await
        {
            error!(error = %e, "Failed to mark video as completed");
            // Still return success since the file was downloaded
        }

        DownloadOutcome::Success {
            video_id,
            file_path: final_path,
            file_size_bytes: result.file_size_bytes,
        }
    }

    fn incomplete_dir(&self) -> PathBuf {
        self.config.output_dir.join(INCOMPLETE_DIR_NAME)
    }

    fn completed_dir(&self) -> PathBuf {
        self.config.output_dir.join(COMPLETED_DIR_NAME)
    }

    async fn move_to_completed(
        source_path: &std::path::Path,
        incomplete_dir: &std::path::Path,
        completed_dir: &std::path::Path,
    ) -> std::io::Result<PathBuf> {
        // Compute relative path from incomplete_dir to preserve subdirectory structure.
        // e.g., incomplete/AliSiddiq/video.mkv -> AliSiddiq/video.mkv -> completed/AliSiddiq/video.mkv
        let relative_path = source_path
            .strip_prefix(incomplete_dir)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

        if relative_path.as_os_str().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Downloaded file path has no filename",
            ));
        }

        let destination_path = completed_dir.join(relative_path);

        // Create parent directories if the template includes subdirectories
        if let Some(parent) = destination_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if tokio::fs::try_exists(&destination_path).await? {
            tokio::fs::remove_file(&destination_path).await?;
        }

        match tokio::fs::rename(source_path, &destination_path).await {
            Ok(()) => Ok(destination_path),
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                tokio::fs::copy(source_path, &destination_path).await?;
                tokio::fs::remove_file(source_path).await?;
                Ok(destination_path)
            }
            Err(error) => Err(error),
        }
    }

    async fn build_template_data(&self) -> OutputTemplateData {
        let publication_time = self.video.published_at.unwrap_or(self.video.created_at);
        let episode_index = self
            .episode_index_for_day(publication_time.date_naive())
            .await
            .unwrap_or(1);

        OutputTemplateData {
            source_name: self.config.source_name.clone(),
            episode_date: publication_time.date_naive(),
            season_year: publication_time.year(),
            episode_index,
            fallback_title: self.video.title.clone(),
        }
    }

    async fn episode_index_for_day(&self, day: chrono::NaiveDate) -> Option<usize> {
        let videos = db::list_videos_for_source(&self.pool, self.config.source_id)
            .await
            .ok()?;

        let mut day_videos: Vec<&Video> = videos
            .iter()
            .filter(|video| video.published_at.unwrap_or(video.created_at).date_naive() == day)
            .collect();

        day_videos.sort_by(|a, b| {
            let a_time = a.published_at.unwrap_or(a.created_at);
            let b_time = b.published_at.unwrap_or(b.created_at);
            a_time
                .cmp(&b_time)
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        });

        day_videos
            .iter()
            .position(|video| video.id == self.video.id)
            .map(|idx| idx + 1)
    }

    /// Handle a download error.
    fn handle_error(&self, error: &YtdlpError) -> DownloadOutcome {
        let video_id = self.video.id;
        let is_rate_limited = matches!(error, YtdlpError::RateLimited(_));
        let error_str = error.to_string();

        if is_rate_limited {
            warn!(error = %error_str, "Download rate limited");
        } else {
            error!(error = %error_str, "Download failed");
        }

        // Note: The supervisor will handle retry scheduling and database updates
        // We just report the outcome here

        DownloadOutcome::Failed {
            video_id,
            error: error_str,
            is_rate_limited,
        }
    }
}

/// Query the current status of a download worker.
pub struct GetStatus;

/// Status information for a download worker.
#[derive(Debug, Clone, Reply)]
pub struct WorkerStatus {
    pub video_id: Ulid,
    pub video_title: String,
    pub platform: String,
}

impl Message<GetStatus> for DownloadWorker {
    type Reply = WorkerStatus;

    async fn handle(
        &mut self,
        _msg: GetStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        WorkerStatus {
            video_id: self.video.id,
            video_title: self.video.title.clone(),
            platform: self.video.platform.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DownloadWorker;
    use tempfile::TempDir;

    #[test]
    fn test_build_video_url_youtube() {
        // We can't easily test the full actor, but we can test URL building logic
        let url = build_url_for_platform("youtube", "dQw4w9WgXcQ");
        assert_eq!(url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }

    #[test]
    fn test_build_video_url_vimeo() {
        let url = build_url_for_platform("vimeo", "123456789");
        assert_eq!(url, "https://vimeo.com/123456789");
    }

    #[test]
    fn test_build_video_url_twitter() {
        let url = build_url_for_platform("twitter", "1234567890");
        assert_eq!(url, "https://twitter.com/i/status/1234567890");
    }

    #[test]
    fn test_build_video_url_full_url() {
        let url = build_url_for_platform("generic", "https://example.com/video/123");
        assert_eq!(url, "https://example.com/video/123");
    }

    // Helper for testing URL building without a full actor
    fn build_url_for_platform(platform: &str, video_id: &str) -> String {
        match platform {
            "youtube" => format!("https://www.youtube.com/watch?v={video_id}"),
            "vimeo" => format!("https://vimeo.com/{video_id}"),
            "twitter" | "x" => format!("https://twitter.com/i/status/{video_id}"),
            "tiktok" => format!("https://www.tiktok.com/@unknown/video/{video_id}"),
            "twitch" => format!("https://www.twitch.tv/videos/{video_id}"),
            _ => {
                if video_id.starts_with("http") {
                    video_id.to_string()
                } else {
                    format!("https://{platform}.com/{video_id}")
                }
            }
        }
    }

    #[tokio::test]
    async fn test_move_to_completed_preserves_subdirectory() {
        let temp = TempDir::new().unwrap();
        let incomplete_dir = temp.path().join("incomplete");
        let completed_dir = temp.path().join("completed");

        // Create subdirectory structure: incomplete/AliSiddiq/video.mkv
        let subdir = incomplete_dir.join("AliSiddiq");
        std::fs::create_dir_all(&subdir).unwrap();
        let source_file = subdir.join("video.mkv");
        std::fs::write(&source_file, b"test content").unwrap();

        let result =
            DownloadWorker::move_to_completed(&source_file, &incomplete_dir, &completed_dir)
                .await
                .unwrap();

        // Should preserve subdirectory: completed/AliSiddiq/video.mkv
        assert_eq!(result, completed_dir.join("AliSiddiq").join("video.mkv"));
        assert!(result.exists());
        assert!(!source_file.exists());
    }

    #[tokio::test]
    async fn test_move_to_completed_flat_file() {
        let temp = TempDir::new().unwrap();
        let incomplete_dir = temp.path().join("incomplete");
        let completed_dir = temp.path().join("completed");

        // Create flat file: incomplete/video.mkv
        std::fs::create_dir_all(&incomplete_dir).unwrap();
        let source_file = incomplete_dir.join("video.mkv");
        std::fs::write(&source_file, b"test content").unwrap();

        let result =
            DownloadWorker::move_to_completed(&source_file, &incomplete_dir, &completed_dir)
                .await
                .unwrap();

        // Should be: completed/video.mkv
        assert_eq!(result, completed_dir.join("video.mkv"));
        assert!(result.exists());
        assert!(!source_file.exists());
    }

    #[tokio::test]
    async fn test_move_to_completed_nested_subdirectories() {
        let temp = TempDir::new().unwrap();
        let incomplete_dir = temp.path().join("incomplete");
        let completed_dir = temp.path().join("completed");

        // Create nested structure: incomplete/Channel/2026/video.mkv
        let subdir = incomplete_dir.join("Channel").join("2026");
        std::fs::create_dir_all(&subdir).unwrap();
        let source_file = subdir.join("video.mkv");
        std::fs::write(&source_file, b"test content").unwrap();

        let result =
            DownloadWorker::move_to_completed(&source_file, &incomplete_dir, &completed_dir)
                .await
                .unwrap();

        // Should preserve full structure: completed/Channel/2026/video.mkv
        assert_eq!(
            result,
            completed_dir.join("Channel").join("2026").join("video.mkv")
        );
        assert!(result.exists());
    }

    #[tokio::test]
    async fn test_move_to_completed_overwrites_existing() {
        let temp = TempDir::new().unwrap();
        let incomplete_dir = temp.path().join("incomplete");
        let completed_dir = temp.path().join("completed");

        // Create source file
        std::fs::create_dir_all(&incomplete_dir).unwrap();
        let source_file = incomplete_dir.join("video.mkv");
        std::fs::write(&source_file, b"new content").unwrap();

        // Create existing destination file
        std::fs::create_dir_all(&completed_dir).unwrap();
        let dest_file = completed_dir.join("video.mkv");
        std::fs::write(&dest_file, b"old content").unwrap();

        let result =
            DownloadWorker::move_to_completed(&source_file, &incomplete_dir, &completed_dir)
                .await
                .unwrap();

        assert_eq!(result, dest_file);
        assert_eq!(std::fs::read_to_string(&result).unwrap(), "new content");
    }

    #[tokio::test]
    async fn test_move_to_completed_invalid_prefix() {
        let temp = TempDir::new().unwrap();
        let incomplete_dir = temp.path().join("incomplete");
        let completed_dir = temp.path().join("completed");
        let other_dir = temp.path().join("other");

        // Create file in a different directory
        std::fs::create_dir_all(&other_dir).unwrap();
        let source_file = other_dir.join("video.mkv");
        std::fs::write(&source_file, b"test content").unwrap();

        // Should fail because source_file is not under incomplete_dir
        let result =
            DownloadWorker::move_to_completed(&source_file, &incomplete_dir, &completed_dir).await;

        assert!(result.is_err());
    }
}
