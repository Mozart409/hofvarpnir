//! Wrapper around the `yt-dlp` crate for video downloading and metadata extraction.
//!
//! Provides:
//! - Video metadata fetching via the generic extractor
//! - Playlist/channel indexing for source discovery
//! - Video downloading with progress callbacks
//! - Quality selection based on profile settings

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument};
use ulid::Ulid;
use yt_dlp::Downloader;
use yt_dlp::client::deps::Libraries;
use yt_dlp::extractor::ExtractorConfig;
use yt_dlp::extractor::VideoExtractor;
use yt_dlp::model::Video as YtVideo;
use yt_dlp::model::playlist::{Playlist, PlaylistEntry as YtPlaylistEntry};
use yt_dlp::model::selector::{
    AudioCodecPreference, AudioQuality, VideoCodecPreference, VideoQuality,
};

use crate::domain::profile::{OutputPreset, Quality};
use crate::domain::video::DownloadProgress;

/// Errors that can occur during yt-dlp operations.
#[derive(Debug, thiserror::Error)]
pub enum YtdlpError {
    /// Failed to initialize the downloader.
    #[error("Failed to initialize yt-dlp downloader: {0}")]
    InitializationError(String),

    /// Failed to fetch video metadata.
    #[error("Failed to fetch video metadata: {0}")]
    MetadataError(String),

    /// Failed to fetch playlist/channel info.
    #[error("Failed to fetch playlist info: {0}")]
    PlaylistError(String),

    /// Preferred preset/quality could not be satisfied after fallback exhaustion.
    #[error(
        "Failed to download video: format fallback exhausted for preset {preset:?} and quality {quality:?} (last_stage={last_stage:?}): {detail}"
    )]
    FormatUnavailable {
        preset: OutputPreset,
        quality: Quality,
        last_stage: Option<FallbackStage>,
        detail: String,
    },

    /// Defensive error when preset-to-selector mapping is invalid.
    #[error(
        "Failed to download video: invalid format preset for preset {preset:?} and quality {quality:?}: {detail}"
    )]
    FormatInvalidPreset {
        preset: OutputPreset,
        quality: Quality,
        detail: String,
    },

    /// yt-dlp execution failed after format selection.
    #[error(
        "Failed to download video: execution failed for preset {preset:?} and quality {quality:?} (stage={stage:?}): {detail}"
    )]
    DownloadExecutionFailed {
        preset: OutputPreset,
        quality: Quality,
        stage: Option<FallbackStage>,
        detail: String,
    },

    /// Video not found or unavailable.
    #[error("Video unavailable: {0}")]
    VideoUnavailable(String),

    /// Rate limited by the platform.
    #[error("Rate limited (HTTP 429): {0}")]
    RateLimited(String),
}

impl YtdlpError {
    pub const DOWNLOAD_FORMAT_UNAVAILABLE: &str = "DOWNLOAD_FORMAT_UNAVAILABLE";
    pub const DOWNLOAD_FORMAT_INVALID_PRESET: &str = "DOWNLOAD_FORMAT_INVALID_PRESET";
    pub const DOWNLOAD_EXECUTION_FAILED: &str = "DOWNLOAD_EXECUTION_FAILED";

    #[must_use]
    pub const fn machine_code(&self) -> Option<&'static str> {
        match self {
            Self::FormatUnavailable { .. } => Some(Self::DOWNLOAD_FORMAT_UNAVAILABLE),
            Self::FormatInvalidPreset { .. } => Some(Self::DOWNLOAD_FORMAT_INVALID_PRESET),
            Self::DownloadExecutionFailed { .. } => Some(Self::DOWNLOAD_EXECUTION_FAILED),
            Self::InitializationError(_)
            | Self::MetadataError(_)
            | Self::PlaylistError(_)
            | Self::VideoUnavailable(_)
            | Self::RateLimited(_) => None,
        }
    }

    #[must_use]
    pub const fn fallback_stage(&self) -> Option<FallbackStage> {
        match self {
            Self::FormatUnavailable { last_stage, .. } => *last_stage,
            Self::DownloadExecutionFailed { stage, .. } => *stage,
            Self::InitializationError(_)
            | Self::MetadataError(_)
            | Self::PlaylistError(_)
            | Self::FormatInvalidPreset { .. }
            | Self::VideoUnavailable(_)
            | Self::RateLimited(_) => None,
        }
    }
}

impl From<yt_dlp::error::Error> for YtdlpError {
    fn from(err: yt_dlp::error::Error) -> Self {
        let msg = err.to_string();
        let msg_lower = msg.to_lowercase();

        // Check for rate limiting patterns (YouTube bot detection, HTTP 429, etc.)
        if msg.contains("429")
            || msg_lower.contains("rate limit")
            || msg_lower.contains("sign in to confirm")
            || msg_lower.contains("confirm you're not a bot")
            || msg_lower.contains("too many requests")
        {
            return Self::RateLimited(msg);
        }

        if msg_lower.contains("unavailable")
            || msg_lower.contains("private")
            || msg_lower.contains("removed")
            || msg_lower.contains("not found")
        {
            return Self::VideoUnavailable(msg);
        }

        Self::MetadataError(msg)
    }
}

/// Metadata extracted from a video, suitable for database storage.
#[derive(Debug, Clone)]
pub struct VideoMetadata {
    /// Platform identifier (e.g., "youtube", "vimeo").
    pub platform: String,
    /// Platform-specific video ID.
    pub platform_video_id: String,
    /// Video title.
    pub title: String,
    /// Video description.
    pub description: Option<String>,
    /// Duration in seconds.
    pub duration_secs: Option<i64>,
    /// Publication timestamp.
    pub published_at: Option<DateTime<Utc>>,
    /// Thumbnail URL.
    pub thumbnail_url: Option<String>,
    /// Whether this is a live stream.
    pub is_live: bool,
    /// Whether this was originally a live stream.
    pub was_live: bool,
    /// Media type (video, short, etc.).
    pub media_type: Option<String>,
}

impl VideoMetadata {
    /// Convert from yt-dlp's Video type.
    fn from_yt_video(video: &YtVideo, platform: &str) -> Self {
        Self {
            platform: platform.to_string(),
            platform_video_id: video.id.clone(),
            title: video.title.clone(),
            description: video.description.clone(),
            duration_secs: video.duration,
            published_at: video
                .upload_date
                .and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
            thumbnail_url: video.thumbnail.clone(),
            is_live: video.is_live.unwrap_or(false),
            was_live: video.was_live.unwrap_or(false),
            media_type: video.media_type.clone(),
        }
    }

    /// Check if this is a `YouTube` Short.
    #[must_use]
    pub fn is_short(&self) -> bool {
        self.media_type.as_deref() == Some("short")
    }
}

/// Entry from a playlist or channel, used during source indexing.
#[derive(Debug, Clone)]
pub struct PlaylistEntry {
    /// Platform-specific video ID.
    pub platform_video_id: String,
    /// Video title.
    pub title: String,
    /// Video URL.
    pub url: String,
    /// Duration in seconds.
    pub duration_secs: Option<i64>,
    /// Thumbnail URL.
    pub thumbnail_url: Option<String>,
}

impl From<&YtPlaylistEntry> for PlaylistEntry {
    #[allow(clippy::cast_possible_truncation)]
    fn from(entry: &YtPlaylistEntry) -> Self {
        Self {
            platform_video_id: entry.id.clone(),
            title: entry.title.clone(),
            url: entry.url.clone(),
            duration_secs: entry.duration.map(|d| d as i64),
            thumbnail_url: entry.thumbnail.clone(),
        }
    }
}

/// Result of indexing a source (channel or playlist).
#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Platform identifier.
    pub platform: String,
    /// Source title (channel/playlist name).
    pub title: String,
    /// Source description.
    pub description: Option<String>,
    /// List of video entries.
    pub entries: Vec<PlaylistEntry>,
    /// Total count (may be more than entries if paginated).
    pub total_count: Option<usize>,
    /// Channel/uploader ID (e.g., `YouTube` channel ID like `UCxxxxx`).
    pub channel_id: Option<String>,
    /// Channel/uploader name.
    pub channel_name: Option<String>,
    /// URL to the best thumbnail from the first video (used for poster).
    pub thumbnail_url: Option<String>,
}

impl IndexResult {
    fn from_playlist(playlist: &Playlist, platform: &str) -> Self {
        // Try to get thumbnail from first entry
        let thumbnail_url = playlist.entries.first().and_then(|e| e.thumbnail.clone());

        Self {
            platform: platform.to_string(),
            title: playlist.title.clone(),
            description: playlist.description.clone(),
            entries: playlist.entries.iter().map(PlaylistEntry::from).collect(),
            total_count: playlist.video_count,
            channel_id: playlist.uploader_id.clone(),
            channel_name: playlist.uploader.clone(),
            thumbnail_url,
        }
    }
}

/// Result of a completed download.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// Path to the downloaded file.
    pub file_path: PathBuf,
    /// File size in bytes.
    pub file_size_bytes: i64,
}

/// Parameters required to run a single download.
pub struct DownloadRequest<'a> {
    pub url: &'a str,
    pub output_dir: &'a Path,
    pub naming_template: &'a str,
    pub template_data: &'a OutputTemplateData,
    pub format_policy: &'a FormatPolicy,
    pub video_id: Ulid,
    pub progress_tx: Option<mpsc::Sender<DownloadProgress>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackStage {
    PreferredCodecPair,
    PreferredVideoCodec,
    AnyCodec,
}

#[derive(Debug, Clone)]
pub struct FormatPolicy {
    pub quality: Quality,
    pub preset: OutputPreset,
    pub audio_quality: AudioQuality,
    pub video_codec: VideoCodecPreference,
    pub audio_codec: AudioCodecPreference,
    pub container_ext: &'static str,
}

impl FormatPolicy {
    #[must_use]
    pub fn from(quality: &Quality, preset: &OutputPreset) -> Self {
        let (video_codec, audio_codec, container_ext) = match preset {
            OutputPreset::Auto => (VideoCodecPreference::Any, AudioCodecPreference::Any, "mkv"),
            OutputPreset::Browser => (VideoCodecPreference::AVC1, AudioCodecPreference::AAC, "mp4"),
            OutputPreset::Tv => (
                VideoCodecPreference::Custom("hevc".to_string()),
                AudioCodecPreference::AAC,
                "mp4",
            ),
        };

        Self {
            quality: quality.clone(),
            preset: preset.clone(),
            audio_quality: AudioQuality::Best,
            video_codec,
            audio_codec,
            container_ext,
        }
    }
}

/// Context values used to render advanced output templates.
#[derive(Debug, Clone)]
pub struct OutputTemplateData {
    pub source_name: String,
    pub season_year: i32,
    pub episode_date: NaiveDate,
    pub episode_index: usize,
    pub fallback_title: String,
}

/// Client for interacting with yt-dlp.
///
/// This wraps the yt-dlp crate's `Downloader` and provides a higher-level API
/// tailored for the hofvarpnir use case.
pub struct YtdlpClient {
    downloader: Arc<Downloader>,
    platform: String,
}

impl std::fmt::Debug for YtdlpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YtdlpClient")
            .field("platform", &self.platform)
            .field("output_dir", &self.downloader.output_dir())
            .finish()
    }
}

impl YtdlpClient {
    /// Create a new yt-dlp client.
    ///
    /// # Arguments
    ///
    /// * `ytdlp_path` - Path to the yt-dlp executable
    /// * `ffmpeg_path` - Path to the ffmpeg executable (optional, uses system ffmpeg if None)
    /// * `output_dir` - Default output directory for downloads
    ///
    /// # Errors
    ///
    /// Returns an error if the downloader fails to initialize.
    pub async fn new(
        ytdlp_path: impl Into<PathBuf>,
        ffmpeg_path: Option<PathBuf>,
        output_dir: impl Into<PathBuf>,
    ) -> Result<Self, YtdlpError> {
        let ytdlp_path = ytdlp_path.into();
        let ffmpeg_path = ffmpeg_path.unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let output_dir = output_dir.into();

        debug!(
            ytdlp = %ytdlp_path.display(),
            ffmpeg = %ffmpeg_path.display(),
            output = %output_dir.display(),
            "Initializing yt-dlp client"
        );

        let libraries = Libraries::new(ytdlp_path, ffmpeg_path);
        let downloader = Downloader::builder(libraries, output_dir)
            .build()
            .await
            .map_err(|e| YtdlpError::InitializationError(e.to_string()))?;

        Ok(Self {
            downloader: Arc::new(downloader),
            platform: "youtube".to_string(), // Default, will be detected per-URL
        })
    }

    /// Fetch metadata for a single video.
    ///
    /// # Arguments
    ///
    /// * `url` - The video URL
    ///
    /// # Errors
    ///
    /// Returns an error if metadata extraction fails.
    #[instrument(skip(self), fields(url = %url))]
    pub async fn fetch_video_metadata(&self, url: &str) -> Result<VideoMetadata, YtdlpError> {
        info!("Fetching video metadata");

        let extractor = self.downloader.generic_extractor();
        let video = extractor.fetch_video(url).await?;

        // Detect platform from URL or extractor
        let platform = Self::detect_platform(url);

        Ok(VideoMetadata::from_yt_video(&video, &platform))
    }

    /// Index a source (channel or playlist) to discover videos.
    ///
    /// This uses `--flat-playlist` mode to quickly enumerate videos
    /// without fetching full metadata for each one.
    ///
    /// # Arguments
    ///
    /// * `url` - The channel or playlist URL
    ///
    /// # Errors
    ///
    /// Returns an error if indexing fails.
    #[instrument(skip(self), fields(url = %url))]
    pub async fn index_source(&self, url: &str) -> Result<IndexResult, YtdlpError> {
        info!("Indexing source");

        let platform = Self::detect_platform(url);

        let mut extractor = self.downloader.generic_extractor().clone();
        if platform == "youtube" && url.contains("list=") {
            extractor.with_arg("--playlist-reverse".to_string());
        }

        let playlist = extractor
            .fetch_playlist(url)
            .await
            .map_err(|e| YtdlpError::PlaylistError(e.to_string()))?;

        debug!(
            title = %playlist.title,
            entries = playlist.entries.len(),
            uploader_id = ?playlist.uploader_id,
            "Source indexed"
        );

        let mut result = IndexResult::from_playlist(&playlist, &platform);

        // For YouTube channels, if no thumbnail from playlist, try fetching first video's metadata
        if platform == "youtube"
            && result.thumbnail_url.is_none()
            && let Some(first_entry) = playlist.entries.first()
            && let Ok(video) = extractor.fetch_video(&first_entry.url).await
            && let Some(thumbnail) = video.thumbnail
        {
            debug!(thumbnail = %thumbnail, "Found thumbnail from first video");
            result.thumbnail_url = Some(thumbnail);
        }

        Ok(result)
    }

    /// Download a video with progress reporting.
    ///
    /// # Arguments
    ///
    /// * `request` - Download request parameters and template context
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails.
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self, request), fields(url = %request.url, policy = ?request.format_policy))]
    pub async fn download_video(
        &self,
        request: DownloadRequest<'_>,
    ) -> Result<DownloadResult, YtdlpError> {
        info!("Starting video download");

        // First fetch video info
        let extractor = self.downloader.generic_extractor();
        let video = extractor.fetch_video(request.url).await?;

        let platform_video_id = video.id.clone();

        // Build output filename from template.
        // We resolve placeholders ourselves because this library path API expects
        // a concrete output path, not yt-dlp style template placeholders.
        let resolved_title = if video.title.trim().is_empty() {
            request.template_data.fallback_title.as_str()
        } else {
            &video.title
        };
        let output_relative_path = render_output_relative_path(
            request.naming_template,
            resolved_title,
            &platform_video_id,
            request.template_data,
            request.format_policy.container_ext,
            !matches!(request.format_policy.quality, Quality::AudioOnly),
        );
        let output_path = request.output_dir.join(output_relative_path);

        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                YtdlpError::DownloadExecutionFailed {
                    preset: request.format_policy.preset.clone(),
                    quality: request.format_policy.quality.clone(),
                    stage: None,
                    detail: format!("Failed to create output subdirectory: {e}"),
                }
            })?;
        }

        let attempts = fallback_attempts(request.format_policy);
        let (result_path, selected_stage) =
            execute_fallback_attempts(&attempts, request.format_policy, |attempt| {
                let video_quality = attempt.video_quality;
                let video_codec = attempt.video_codec.clone();
                let audio_codec = attempt.audio_codec.clone();
                let video_ref = video.clone();
                let output_path_ref = output_path.clone();
                async move {
                    self.downloader
                        .download(&video_ref, &output_path_ref)
                        .video_quality(video_quality)
                        .audio_quality(request.format_policy.audio_quality)
                        .video_codec(video_codec)
                        .audio_codec(audio_codec)
                        .execute()
                        .await
                        .map_err(|err| err.to_string())
                }
            })
            .await?;

        debug!(stage = ?selected_stage, "Selected format fallback stage");

        // Get file size
        let file_size = tokio::fs::metadata(&result_path)
            .await
            .map_or(0, |m| i64::try_from(m.len()).unwrap_or(i64::MAX));

        // Send final progress update
        if let Some(tx) = request.progress_tx {
            let size_u64 = u64::try_from(file_size).unwrap_or(0);
            let _ = tx
                .send(DownloadProgress {
                    video_id: request.video_id,
                    platform_video_id,
                    percent: 100.0,
                    speed: None,
                    eta: None,
                    downloaded_bytes: Some(size_u64),
                    total_bytes: Some(size_u64),
                })
                .await;
        }

        info!(path = %result_path.display(), size = file_size, "Download completed");

        Ok(DownloadResult {
            file_path: result_path,
            file_size_bytes: file_size,
        })
    }

    /// Download a video to a specific directory with progress callbacks.
    ///
    /// This is a convenience method that constructs the full output path.
    ///
    /// # Arguments
    ///
    /// * `request` - Download request parameters and template context
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails.
    pub async fn download_video_to_dir(
        &self,
        request: DownloadRequest<'_>,
    ) -> Result<DownloadResult, YtdlpError> {
        // Create output directory if needed
        tokio::fs::create_dir_all(request.output_dir)
            .await
            .map_err(|e| YtdlpError::DownloadExecutionFailed {
                preset: request.format_policy.preset.clone(),
                quality: request.format_policy.quality.clone(),
                stage: None,
                detail: format!("Failed to create output dir: {e}"),
            })?;

        self.download_video(request).await
    }

    /// Detect the platform from a URL.
    fn detect_platform(url: &str) -> String {
        let url_lower = url.to_lowercase();

        if url_lower.contains("youtube.com") || url_lower.contains("youtu.be") {
            "youtube".to_string()
        } else if url_lower.contains("vimeo.com") {
            "vimeo".to_string()
        } else if url_lower.contains("twitter.com") || url_lower.contains("x.com") {
            "twitter".to_string()
        } else if url_lower.contains("tiktok.com") {
            "tiktok".to_string()
        } else if url_lower.contains("instagram.com") {
            "instagram".to_string()
        } else if url_lower.contains("twitch.tv") {
            "twitch".to_string()
        } else {
            // Default to generic
            "generic".to_string()
        }
    }

    /// Get the output directory.
    #[must_use]
    pub fn output_dir(&self) -> &Path {
        self.downloader.output_dir()
    }

    /// Shutdown the client gracefully.
    pub fn shutdown(&self) {
        self.downloader.shutdown();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FallbackAttempt {
    stage: FallbackStage,
    video_quality: VideoQuality,
    video_codec: VideoCodecPreference,
    audio_codec: AudioCodecPreference,
}

fn fallback_attempts(policy: &FormatPolicy) -> Vec<FallbackAttempt> {
    let quality_levels = quality_fallback_chain(&policy.quality);
    let mut attempts = Vec::new();

    for video_quality in quality_levels {
        if policy.preset == OutputPreset::Auto {
            attempts.push(FallbackAttempt {
                stage: FallbackStage::AnyCodec,
                video_quality,
                video_codec: VideoCodecPreference::Any,
                audio_codec: AudioCodecPreference::Any,
            });
            continue;
        }

        attempts.push(FallbackAttempt {
            stage: FallbackStage::PreferredCodecPair,
            video_quality,
            video_codec: policy.video_codec.clone(),
            audio_codec: policy.audio_codec.clone(),
        });
        attempts.push(FallbackAttempt {
            stage: FallbackStage::PreferredVideoCodec,
            video_quality,
            video_codec: policy.video_codec.clone(),
            audio_codec: AudioCodecPreference::Any,
        });
        attempts.push(FallbackAttempt {
            stage: FallbackStage::AnyCodec,
            video_quality,
            video_codec: VideoCodecPreference::Any,
            audio_codec: AudioCodecPreference::Any,
        });
    }

    let mut deduped = Vec::new();
    for attempt in attempts {
        if !deduped.contains(&attempt) {
            deduped.push(attempt);
        }
    }

    deduped
}

async fn execute_fallback_attempts<F, Fut>(
    attempts: &[FallbackAttempt],
    policy: &FormatPolicy,
    mut execute: F,
) -> Result<(PathBuf, FallbackStage), YtdlpError>
where
    F: FnMut(&FallbackAttempt) -> Fut,
    Fut: Future<Output = Result<PathBuf, String>>,
{
    let mut last_error = None;
    let mut last_stage = None;

    for attempt in attempts {
        last_stage = Some(attempt.stage);
        debug!(
            stage = ?attempt.stage,
            video_quality = ?attempt.video_quality,
            video_codec = ?attempt.video_codec,
            audio_codec = ?attempt.audio_codec,
            preset = ?policy.preset,
            quality = ?policy.quality,
            "Attempting format selection"
        );

        match execute(attempt).await {
            Ok(path) => return Ok((path, attempt.stage)),
            Err(err) => {
                last_error = Some(err);
                debug!(
                    stage = ?attempt.stage,
                    error = last_error.as_deref(),
                    "Format selection attempt failed"
                );
            }
        }
    }

    let detail = last_error.unwrap_or_else(|| "no compatible format found".to_string());
    Err(YtdlpError::FormatUnavailable {
        preset: policy.preset.clone(),
        quality: policy.quality.clone(),
        last_stage,
        detail,
    })
}

fn quality_fallback_chain(quality: &Quality) -> Vec<VideoQuality> {
    match quality {
        Quality::Best => vec![
            VideoQuality::Best,
            VideoQuality::High,
            VideoQuality::Medium,
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::Q4320p => vec![
            VideoQuality::CustomHeight(4320),
            VideoQuality::CustomHeight(2160),
            VideoQuality::CustomHeight(1440),
            VideoQuality::CustomHeight(1080),
            VideoQuality::CustomHeight(720),
            VideoQuality::CustomHeight(480),
            VideoQuality::High,
            VideoQuality::Medium,
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::Q2160p => vec![
            VideoQuality::CustomHeight(2160),
            VideoQuality::CustomHeight(1440),
            VideoQuality::CustomHeight(1080),
            VideoQuality::CustomHeight(720),
            VideoQuality::CustomHeight(480),
            VideoQuality::High,
            VideoQuality::Medium,
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::Q1440p => vec![
            VideoQuality::CustomHeight(1440),
            VideoQuality::CustomHeight(1080),
            VideoQuality::CustomHeight(720),
            VideoQuality::CustomHeight(480),
            VideoQuality::High,
            VideoQuality::Medium,
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::Q1080p => vec![
            VideoQuality::CustomHeight(1080),
            VideoQuality::CustomHeight(720),
            VideoQuality::CustomHeight(480),
            VideoQuality::High,
            VideoQuality::Medium,
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::Q720p => vec![
            VideoQuality::CustomHeight(720),
            VideoQuality::CustomHeight(480),
            VideoQuality::Medium,
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::Q480p => vec![
            VideoQuality::CustomHeight(480),
            VideoQuality::Low,
            VideoQuality::Worst,
        ],
        Quality::AudioOnly => vec![VideoQuality::Worst],
    }
}

/// Render a relative output path from a user template.
///
/// Supported placeholders:
/// - `{title}` / `{{ title }}` -> video title (sanitized)
/// - `{id}` / `{{ id }}` -> platform video ID (sanitized)
/// - `{ext}` / `%(ext)s` -> final container extension (when forced)
/// - `{{ source_custom_name/or default }}` -> source display name
/// - `{{ season_by_year__episode_by_date_and_index }}` -> `SYYYYEYYYYMMDD-III`
#[allow(clippy::literal_string_with_formatting_args)]
fn render_output_relative_path(
    template: &str,
    title: &str,
    platform_video_id: &str,
    template_data: &OutputTemplateData,
    container_ext: &str,
    force_container_ext: bool,
) -> PathBuf {
    let safe_title = sanitize_filename_component(title);
    let safe_id = sanitize_filename_component(platform_video_id);
    let safe_source = sanitize_filename_component(&template_data.source_name);
    let year = template_data.episode_date.format("%Y").to_string();
    let season_episode = format!(
        "S{}E{}-{:03}",
        template_data.season_year,
        template_data.episode_date.format("%Y%m%d"),
        template_data.episode_index
    );

    // Double-brace placeholders MUST be replaced before single-brace ones,
    // otherwise `{{ext}}` is partially matched by `{ext}` leaving residual braces.
    let ext = if force_container_ext {
        container_ext
    } else {
        ""
    };

    let mut rendered = template
        .replace("{{ ext }}", ext)
        .replace("{{ext}}", ext)
        .replace("{{ title }}", &safe_title)
        .replace("{{title}}", &safe_title)
        .replace("{{ id }}", &safe_id)
        .replace("{{id}}", &safe_id)
        .replace("{{ year }}", &year)
        .replace("{{year}}", &year)
        .replace("{{ source_custom_name/or default }}", &safe_source)
        .replace("{{source_custom_name/or default}}", &safe_source)
        .replace("{{ source_custom_name_or_default }}", &safe_source)
        .replace("{{source_custom_name_or_default}}", &safe_source)
        .replace(
            "{{ season_by_year__episode_by_date_and_index }}",
            &season_episode,
        )
        .replace(
            "{{season_by_year__episode_by_date_and_index}}",
            &season_episode,
        )
        .replace("{title}", &safe_title)
        .replace("{id}", &safe_id)
        .replace("{year}", &year)
        .replace("{ext}", ext)
        .replace("%(ext)s", ext);

    if rendered.trim().is_empty() {
        rendered = if force_container_ext {
            format!("{safe_title}-{safe_id}.{container_ext}")
        } else {
            format!("{safe_title}-{safe_id}")
        };
    }

    let rendered = rendered.replace('\\', "/");
    let mut segments: Vec<String> = rendered
        .split('/')
        .map(str::trim)
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .map(sanitize_filename_component)
        .filter(|segment| !segment.is_empty())
        .collect();

    if segments.is_empty() {
        segments.push(if force_container_ext {
            format!("{safe_title}-{safe_id}.{container_ext}")
        } else {
            format!("{safe_title}-{safe_id}")
        });
    }

    // SAFETY: segments is guaranteed non-empty - we push a fallback above if it was empty
    let Some(last_segment) = segments.last_mut() else {
        unreachable!("segments is guaranteed to contain at least one segment")
    };

    if force_container_ext {
        let has_container_extension = Path::new(last_segment)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(container_ext));
        if !has_container_extension {
            last_segment.push('.');
            last_segment.push_str(container_ext);
        }

        let duplicate = format!(".{container_ext}.{container_ext}");
        while last_segment
            .to_ascii_lowercase()
            .ends_with(&duplicate.to_ascii_lowercase())
        {
            last_segment.truncate(last_segment.len().saturating_sub(container_ext.len() + 1));
        }
    } else {
        *last_segment = last_segment.trim_end_matches('.').to_string();
        if last_segment.is_empty() {
            *last_segment = format!("{safe_title}-{safe_id}");
        }
    }

    segments
        .into_iter()
        .fold(PathBuf::new(), |mut path, segment| {
            path.push(segment);
            path
        })
}

/// Validate a user-provided naming template before persisting profile changes.
///
/// Allowed placeholders:
/// - `{title}`, `{id}`, `{ext}`
/// - `%(ext)s`
/// - `{{ title }}`, `{{ id }}`, `{{ ext }}`
/// - `{{ source_custom_name/or default }}`
/// - `{{ season_by_year__episode_by_date_and_index }}`
///
/// # Errors
///
/// Returns an error if the template is empty or contains invalid placeholders.
pub fn validate_output_template(template: &str) -> Result<(), String> {
    let template = template.trim();
    if template.is_empty() {
        return Err("Naming template cannot be empty".to_string());
    }

    if let Some(raw) = find_unknown_double_brace_placeholder(template) {
        return Err(format!(
            "Unsupported template placeholder '{{{{ {raw} }}}}'. Allowed placeholders: title, id, ext, year, source_custom_name/or default, season_by_year__episode_by_date_and_index"
        ));
    }

    if let Some(raw) = find_unknown_single_brace_placeholder(template) {
        return Err(format!(
            "Unsupported template placeholder '{{{raw}}}'. Allowed placeholders: title, id, ext, year"
        ));
    }

    let normalized = template.replace('\\', "/");
    for segment in normalized.split('/') {
        let trimmed = segment.trim();
        if trimmed == "." || trimmed == ".." {
            return Err(
                "Naming template cannot contain path traversal segments ('.' or '..')".to_string(),
            );
        }
    }

    Ok(())
}

fn find_unknown_double_brace_placeholder(template: &str) -> Option<String> {
    const ALLOWED: &[&str] = &[
        "title",
        "id",
        "ext",
        "year",
        "source_custom_name/or default",
        "source_custom_name_or_default",
        "season_by_year__episode_by_date_and_index",
    ];

    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            return Some("<unclosed-double-brace-placeholder>".to_string());
        };
        let placeholder = after_start[..end].trim();
        if !ALLOWED.contains(&placeholder) {
            return Some(placeholder.to_string());
        }
        rest = &after_start[end + 2..];
    }

    None
}

fn find_unknown_single_brace_placeholder(template: &str) -> Option<String> {
    const ALLOWED: &[&str] = &["title", "id", "ext", "year"];

    let mut rest = template;
    while let Some(start) = rest.find('{') {
        if rest[start..].starts_with("{{") {
            rest = &rest[start + 2..];
            continue;
        }

        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            return Some("<unclosed-single-brace-placeholder>".to_string());
        };
        let placeholder = after_start[..end].trim();
        if !ALLOWED.contains(&placeholder) {
            return Some(placeholder.to_string());
        }
        rest = &after_start[end + 1..];
    }

    None
}

/// Sanitize text so it is safe as a single filename component.
#[must_use]
pub fn sanitize_filename_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    sanitized.trim().to_string()
}

/// Filter playlist entries based on profile settings.
///
/// Note: Full filtering by date and livestream status requires fetching
/// complete metadata for each video. This function applies heuristic
/// filtering based on available playlist entry data.
#[must_use]
pub fn filter_entries(
    entries: &[PlaylistEntry],
    _include_livestreams: bool,
    include_shorts: bool,
    _cutoff_date: Option<chrono::NaiveDate>,
) -> Vec<&PlaylistEntry> {
    entries
        .iter()
        .filter(|entry| {
            // Filter by title heuristics (shorts often have #Shorts in title)
            if !include_shorts && entry.title.to_lowercase().contains("#shorts") {
                return false;
            }

            // We'd need full metadata to filter livestreams and by date
            // For now, include everything that passes the shorts filter
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_data() -> OutputTemplateData {
        OutputTemplateData {
            source_name: "F1 Channel".to_string(),
            season_year: 2026,
            episode_date: NaiveDate::from_ymd_opt(2026, 3, 18).expect("valid date"),
            episode_index: 7,
            fallback_title: "Fallback".to_string(),
        }
    }

    #[test]
    fn test_platform_detection() {
        assert_eq!(
            YtdlpClient::detect_platform("https://www.youtube.com/watch?v=abc"),
            "youtube"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://youtu.be/abc"),
            "youtube"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://vimeo.com/123"),
            "vimeo"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://twitter.com/user/status/123"),
            "twitter"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://x.com/user/status/123"),
            "twitter"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://tiktok.com/@user/video/123"),
            "tiktok"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://instagram.com/p/abc"),
            "instagram"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://twitch.tv/user"),
            "twitch"
        );
        assert_eq!(
            YtdlpClient::detect_platform("https://example.com/video"),
            "generic"
        );
    }

    #[test]
    fn test_render_output_filename_default_template() {
        let output = render_output_relative_path(
            "{title}-{id}.{ext}",
            "Great Race",
            "Hx4xrg6wVNI",
            &template_data(),
            "mkv",
            true,
        );
        assert_eq!(output, PathBuf::from("Great Race-Hx4xrg6wVNI.mkv"));
    }

    #[test]
    fn test_render_output_filename_legacy_double_ext_template() {
        let output = render_output_relative_path(
            "{title}-{id}.{ext}.%(ext)s",
            "F1 overtakes",
            "Hx4xrg6wVNI",
            &template_data(),
            "mkv",
            true,
        );
        assert_eq!(output, PathBuf::from("F1 overtakes-Hx4xrg6wVNI.mkv"));
    }

    #[test]
    fn test_render_output_filename_sanitizes_path_chars() {
        let output = render_output_relative_path(
            "../{title}/{id}",
            "a/b:c*?",
            "id/1",
            &template_data(),
            "mkv",
            true,
        );
        let output_str = output.to_string_lossy();
        assert!(!output_str.contains(".."));
        assert!(!output_str.contains('\\'));
        assert!(
            output
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv"))
        );
    }

    #[test]
    fn test_render_output_filename_advanced_template_with_folders() {
        let output = render_output_relative_path(
            "{{ source_custom_name/or default }}/{{ season_by_year__episode_by_date_and_index }} - {{ title }}.{{ ext }}",
            "F1 Overtake Breakdown",
            "Hx4xrg6wVNI",
            &template_data(),
            "mkv",
            true,
        );

        assert_eq!(
            output,
            PathBuf::from("F1 Channel/S2026E20260318-007 - F1 Overtake Breakdown.mkv")
        );
    }

    #[test]
    fn test_render_output_filename_double_brace_no_spaces() {
        let output = render_output_relative_path(
            "{{source_custom_name/or default}}/{{title}}.{{ext}}",
            "This shot in the F1 movie could've been a lot better",
            "abc123",
            &template_data(),
            "mkv",
            true,
        );

        assert_eq!(
            output,
            PathBuf::from("F1 Channel/This shot in the F1 movie could've been a lot better.mkv")
        );
    }

    #[test]
    fn test_format_policy_browser_defaults() {
        let policy = FormatPolicy::from(&Quality::Q1080p, &OutputPreset::Browser);

        assert_eq!(policy.preset, OutputPreset::Browser);
        assert!(matches!(policy.video_codec, VideoCodecPreference::AVC1));
        assert!(matches!(policy.audio_codec, AudioCodecPreference::AAC));
        assert_eq!(policy.container_ext, "mp4");
    }

    #[test]
    fn test_quality_fallback_chain_orders_descending() {
        let chain = quality_fallback_chain(&Quality::Q1080p);
        assert_eq!(chain.first(), Some(&VideoQuality::CustomHeight(1080)));
        assert_eq!(chain.last(), Some(&VideoQuality::Worst));
    }

    #[test]
    fn test_fallback_attempts_include_all_stages_for_browser() {
        let policy = FormatPolicy::from(&Quality::Q480p, &OutputPreset::Browser);
        let attempts = fallback_attempts(&policy);

        assert!(
            attempts
                .iter()
                .any(|attempt| attempt.stage == FallbackStage::PreferredCodecPair)
        );
        assert!(
            attempts
                .iter()
                .any(|attempt| attempt.stage == FallbackStage::PreferredVideoCodec)
        );
        assert!(
            attempts
                .iter()
                .any(|attempt| attempt.stage == FallbackStage::AnyCodec)
        );
    }

    #[tokio::test]
    async fn test_fallback_recovers_when_preferred_codec_unavailable() {
        let policy = FormatPolicy::from(&Quality::Q1080p, &OutputPreset::Browser);
        let attempts = fallback_attempts(&policy);

        let result = execute_fallback_attempts(&attempts, &policy, |attempt| {
            let stage = attempt.stage;
            async move {
                if stage == FallbackStage::PreferredCodecPair {
                    return Err("preferred codec not available".to_string());
                }
                Ok(PathBuf::from("/tmp/fallback-success.mp4"))
            }
        })
        .await;

        let (_, selected_stage) = result.expect("fallback should succeed");
        assert_eq!(selected_stage, FallbackStage::PreferredVideoCodec);
    }

    #[tokio::test]
    async fn test_fallback_exhaustion_returns_machine_readable_error_code() {
        let policy = FormatPolicy::from(&Quality::Q1080p, &OutputPreset::Browser);
        let attempts = fallback_attempts(&policy);

        let error = execute_fallback_attempts(&attempts, &policy, |_attempt| async {
            Err("no compatible format".to_string())
        })
        .await
        .expect_err("fallback exhaustion should fail");

        assert_eq!(
            error.machine_code(),
            Some(YtdlpError::DOWNLOAD_FORMAT_UNAVAILABLE)
        );
        assert_eq!(error.fallback_stage(), Some(FallbackStage::AnyCodec));
    }

    #[test]
    fn test_video_metadata_is_short() {
        let meta = VideoMetadata {
            platform: "youtube".to_string(),
            platform_video_id: "abc123".to_string(),
            title: "Test".to_string(),
            description: None,
            duration_secs: Some(30),
            published_at: None,
            thumbnail_url: None,
            is_live: false,
            was_live: false,
            media_type: Some("short".to_string()),
        };
        assert!(meta.is_short());

        let regular = VideoMetadata {
            media_type: Some("video".to_string()),
            ..meta
        };
        assert!(!regular.is_short());
    }

    #[test]
    fn test_filter_entries_excludes_shorts() {
        let entries = vec![
            PlaylistEntry {
                platform_video_id: "1".to_string(),
                title: "Regular Video".to_string(),
                url: "https://youtube.com/watch?v=1".to_string(),
                duration_secs: Some(300),
                thumbnail_url: None,
            },
            PlaylistEntry {
                platform_video_id: "2".to_string(),
                title: "My Cool Short #Shorts".to_string(),
                url: "https://youtube.com/watch?v=2".to_string(),
                duration_secs: Some(30),
                thumbnail_url: None,
            },
        ];

        // With shorts included
        let filtered = filter_entries(&entries, true, true, None);
        assert_eq!(filtered.len(), 2);

        // With shorts excluded
        let filtered = filter_entries(&entries, true, false, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].title, "Regular Video");
    }

    #[test]
    fn test_validate_output_template_accepts_advanced_folders() {
        let template = "{{ source_custom_name/or default }}/{{ season_by_year__episode_by_date_and_index }} - {{ title }}.{{ ext }}";
        assert!(validate_output_template(template).is_ok());
    }

    #[test]
    fn test_validate_output_template_accepts_year_placeholder() {
        let template = "{{source_custom_name/or default}}/{{year}}/{{title}}.{{ext}}";
        assert!(validate_output_template(template).is_ok());
    }

    #[test]
    fn test_render_output_filename_with_year_folder() {
        let output = render_output_relative_path(
            "{{source_custom_name/or default}}/{{year}}/{{title}}.{{ext}}",
            "Monaco GP Highlights",
            "abc123",
            &template_data(),
            "mkv",
            true,
        );

        assert_eq!(
            output,
            PathBuf::from("F1 Channel/2026/Monaco GP Highlights.mkv")
        );
    }

    #[test]
    fn test_validate_output_template_rejects_unknown_placeholder() {
        let template = "{{ source }}/{{ title }}.{{ ext }}";
        let error = validate_output_template(template).expect_err("template should fail");
        assert!(error.contains("Unsupported template placeholder"));
    }

    #[test]
    fn test_validate_output_template_rejects_traversal_segment() {
        let template = "../{{ title }}.{{ ext }}";
        let error = validate_output_template(template).expect_err("template should fail");
        assert!(error.contains("path traversal"));
    }

    #[test]
    fn test_machine_error_code_for_format_unavailable() {
        let error = YtdlpError::FormatUnavailable {
            preset: OutputPreset::Browser,
            quality: Quality::Q1080p,
            last_stage: Some(FallbackStage::AnyCodec),
            detail: "no compatible format found".to_string(),
        };

        assert_eq!(
            error.machine_code(),
            Some(YtdlpError::DOWNLOAD_FORMAT_UNAVAILABLE)
        );
        assert_eq!(error.fallback_stage(), Some(FallbackStage::AnyCodec));
    }

    #[test]
    fn test_machine_error_code_for_download_execution_failed() {
        let error = YtdlpError::DownloadExecutionFailed {
            preset: OutputPreset::Tv,
            quality: Quality::Best,
            stage: Some(FallbackStage::PreferredCodecPair),
            detail: "yt-dlp exited with status 1".to_string(),
        };

        assert_eq!(
            error.machine_code(),
            Some(YtdlpError::DOWNLOAD_EXECUTION_FAILED)
        );
        assert_eq!(
            error.fallback_stage(),
            Some(FallbackStage::PreferredCodecPair)
        );
    }

    #[test]
    fn test_render_output_filename_uses_mp4_when_requested() {
        let output = render_output_relative_path(
            "{title}-{id}.{ext}",
            "Great Race",
            "Hx4xrg6wVNI",
            &template_data(),
            "mp4",
            true,
        );
        assert_eq!(output, PathBuf::from("Great Race-Hx4xrg6wVNI.mp4"));
    }

    #[test]
    fn test_render_output_filename_audio_only_does_not_force_extension() {
        let output = render_output_relative_path(
            "{title}-{id}.{ext}",
            "Great Race",
            "Hx4xrg6wVNI",
            &template_data(),
            "mp4",
            false,
        );
        assert_eq!(output, PathBuf::from("Great Race-Hx4xrg6wVNI"));
    }
}
