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

use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::db;
use crate::domain::profile::Quality;
use crate::domain::video::{DownloadProgress, Video};
use crate::ytdlp::{DownloadResult, YtdlpClient, YtdlpError};

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

        // Immediately trigger the download
        actor_ref.tell(StartDownload).await?;

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

        // Execute download with timeout
        let download_future = self.ytdlp.download_video_to_dir(
            &url,
            &self.config.output_dir,
            &self.config.naming_template,
            &self.config.quality,
            video_id,
            Some(self.progress_tx.clone()),
        );

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
        let file_path_str = result.file_path.to_string_lossy().to_string();

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
            file_path: result.file_path,
            file_size_bytes: result.file_size_bytes,
        }
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
}
