//! Wrapper around the `yt-dlp` crate for video downloading and metadata extraction.
//!
//! Provides:
//! - Video metadata fetching via the generic extractor
//! - Playlist/channel indexing for source discovery
//! - Video downloading with progress callbacks
//! - Quality selection based on profile settings

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument};
use ulid::Ulid;
use yt_dlp::Downloader;
use yt_dlp::client::deps::Libraries;
use yt_dlp::extractor::VideoExtractor;
use yt_dlp::model::Video as YtVideo;
use yt_dlp::model::playlist::{Playlist, PlaylistEntry as YtPlaylistEntry};
use yt_dlp::model::selector::{AudioQuality, VideoQuality};

use crate::domain::profile::Quality;
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

    /// Failed to download video.
    #[error("Failed to download video: {0}")]
    DownloadError(String),

    /// Video not found or unavailable.
    #[error("Video unavailable: {0}")]
    VideoUnavailable(String),

    /// Rate limited by the platform.
    #[error("Rate limited (HTTP 429): {0}")]
    RateLimited(String),
}

impl From<yt_dlp::error::Error> for YtdlpError {
    fn from(err: yt_dlp::error::Error) -> Self {
        let msg = err.to_string();

        // Check for common error patterns
        if msg.contains("429") || msg.to_lowercase().contains("rate limit") {
            return Self::RateLimited(msg);
        }
        if msg.contains("unavailable")
            || msg.contains("private")
            || msg.contains("removed")
            || msg.contains("not found")
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
}

impl IndexResult {
    fn from_playlist(playlist: &Playlist, platform: &str) -> Self {
        Self {
            platform: platform.to_string(),
            title: playlist.title.clone(),
            description: playlist.description.clone(),
            entries: playlist.entries.iter().map(PlaylistEntry::from).collect(),
            total_count: playlist.video_count,
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

        let extractor = self.downloader.generic_extractor();
        let playlist = extractor
            .fetch_playlist(url)
            .await
            .map_err(|e| YtdlpError::PlaylistError(e.to_string()))?;

        let platform = Self::detect_platform(url);

        debug!(
            title = %playlist.title,
            entries = playlist.entries.len(),
            "Source indexed"
        );

        Ok(IndexResult::from_playlist(&playlist, &platform))
    }

    /// Download a video with progress reporting.
    ///
    /// # Arguments
    ///
    /// * `url` - The video URL
    /// * `output_dir` - The output directory
    /// * `naming_template` - Filename template (e.g. `"{title}-{id}.{ext}"`)
    /// * `quality` - The quality setting from the profile
    /// * `video_id` - The internal video ID for progress tracking
    /// * `progress_tx` - Channel to send progress updates
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails.
    #[instrument(skip(self, progress_tx), fields(url = %url, quality = ?quality))]
    pub async fn download_video(
        &self,
        url: &str,
        output_dir: &Path,
        naming_template: &str,
        quality: &Quality,
        video_id: Ulid,
        progress_tx: Option<mpsc::Sender<DownloadProgress>>,
    ) -> Result<DownloadResult, YtdlpError> {
        info!("Starting video download");

        // First fetch video info
        let extractor = self.downloader.generic_extractor();
        let video = extractor.fetch_video(url).await?;

        let platform_video_id = video.id.clone();

        // Build output filename from template.
        // We resolve placeholders ourselves because this library path API expects
        // a concrete output path, not yt-dlp style template placeholders.
        let output_filename =
            render_output_filename(naming_template, &video.title, &platform_video_id);
        let output_path = output_dir.join(output_filename);

        // Build download with quality settings
        let video_quality = quality_to_yt_quality(quality);

        // Use the fluent download builder
        let result_path = self
            .downloader
            .download(&video, &output_path)
            .video_quality(video_quality)
            .audio_quality(AudioQuality::Best)
            .execute()
            .await
            .map_err(|e| YtdlpError::DownloadError(e.to_string()))?;

        // Get file size
        let file_size = tokio::fs::metadata(&result_path)
            .await
            .map_or(0, |m| i64::try_from(m.len()).unwrap_or(i64::MAX));

        // Send final progress update
        if let Some(tx) = progress_tx {
            let size_u64 = u64::try_from(file_size).unwrap_or(0);
            let _ = tx
                .send(DownloadProgress {
                    video_id,
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
    /// * `url` - The video URL
    /// * `output_dir` - The directory to save the file
    /// * `naming_template` - Template for filename (e.g., `"{title}-{id}.{ext}"`)
    /// * `quality` - The quality setting
    /// * `video_id` - Internal video ID for progress tracking
    /// * `progress_tx` - Channel for progress updates
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails.
    pub async fn download_video_to_dir(
        &self,
        url: &str,
        output_dir: &Path,
        naming_template: &str,
        quality: &Quality,
        video_id: Ulid,
        progress_tx: Option<mpsc::Sender<DownloadProgress>>,
    ) -> Result<DownloadResult, YtdlpError> {
        // Create output directory if needed
        tokio::fs::create_dir_all(output_dir)
            .await
            .map_err(|e| YtdlpError::DownloadError(format!("Failed to create output dir: {e}")))?;

        self.download_video(
            url,
            output_dir,
            naming_template,
            quality,
            video_id,
            progress_tx,
        )
        .await
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

/// Convert our Quality enum to yt-dlp's `VideoQuality`.
fn quality_to_yt_quality(quality: &Quality) -> VideoQuality {
    match quality {
        Quality::Best => VideoQuality::Best,
        Quality::Q4320p => VideoQuality::CustomHeight(4320),
        Quality::Q2160p => VideoQuality::CustomHeight(2160),
        Quality::Q1440p => VideoQuality::CustomHeight(1440),
        Quality::Q1080p => VideoQuality::CustomHeight(1080),
        Quality::Q720p => VideoQuality::CustomHeight(720),
        Quality::Q480p => VideoQuality::CustomHeight(480),
        Quality::AudioOnly => VideoQuality::Worst, // Audio-only handled differently
    }
}

/// Render a filename from a user template.
///
/// Supported placeholders:
/// - `{title}` -> video title (sanitized)
/// - `{id}` -> platform video ID (sanitized)
/// - `{ext}` / `%(ext)s` -> final container extension (`mkv`)
fn render_output_filename(template: &str, title: &str, platform_video_id: &str) -> String {
    let safe_title = sanitize_filename_component(title);
    let safe_id = sanitize_filename_component(platform_video_id);

    let mut rendered = template
        .replace("{title}", &safe_title)
        .replace("{id}", &safe_id)
        .replace("{ext}", "mkv")
        .replace("%(ext)s", "mkv");

    // Prevent path traversal / nested directories from templates.
    rendered = sanitize_filename_component(&rendered);

    if rendered.is_empty() {
        rendered = format!("{safe_title}-{safe_id}.mkv");
    }

    let is_mkv_extension = std::path::Path::new(&rendered)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv"));
    if !is_mkv_extension {
        rendered.push_str(".mkv");
    }

    // Handle legacy templates that accidentally specify extension twice.
    while rendered.ends_with(".mkv.mkv") {
        rendered.truncate(rendered.len().saturating_sub(4));
    }

    rendered
}

/// Sanitize text so it is safe as a single filename component.
fn sanitize_filename_component(value: &str) -> String {
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
        let output = render_output_filename("{title}-{id}.{ext}", "Great Race", "Hx4xrg6wVNI");
        assert_eq!(output, "Great Race-Hx4xrg6wVNI.mkv");
    }

    #[test]
    fn test_render_output_filename_legacy_double_ext_template() {
        let output =
            render_output_filename("{title}-{id}.{ext}.%(ext)s", "F1 overtakes", "Hx4xrg6wVNI");
        assert_eq!(output, "F1 overtakes-Hx4xrg6wVNI.mkv");
    }

    #[test]
    fn test_render_output_filename_sanitizes_path_chars() {
        let output = render_output_filename("../{title}/{id}", "a/b:c*?", "id/1");
        assert!(!output.contains('/'));
        assert!(!output.contains('\\'));
        assert!(
            std::path::Path::new(&output)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv"))
        );
    }

    #[test]
    fn test_quality_conversion() {
        assert!(matches!(
            quality_to_yt_quality(&Quality::Best),
            VideoQuality::Best
        ));
        assert!(matches!(
            quality_to_yt_quality(&Quality::Q1080p),
            VideoQuality::CustomHeight(1080)
        ));
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
            ..meta.clone()
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
}
