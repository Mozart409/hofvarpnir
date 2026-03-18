//! Wrapper around the `yt-dlp` crate for video downloading and metadata extraction.
//!
//! Provides:
//! - Video metadata fetching via the generic extractor
//! - Playlist/channel indexing for source discovery
//! - Video downloading with progress callbacks
//! - Quality selection based on profile settings

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
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

/// Parameters required to run a single download.
pub struct DownloadRequest<'a> {
    pub url: &'a str,
    pub output_dir: &'a Path,
    pub naming_template: &'a str,
    pub template_data: &'a OutputTemplateData,
    pub quality: &'a Quality,
    pub video_id: Ulid,
    pub progress_tx: Option<mpsc::Sender<DownloadProgress>>,
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
    /// * `request` - Download request parameters and template context
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails.
    #[instrument(skip(self, request), fields(url = %request.url, quality = ?request.quality))]
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
        );
        let output_path = request.output_dir.join(output_relative_path);

        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                YtdlpError::DownloadError(format!("Failed to create output subdirectory: {e}"))
            })?;
        }

        // Build download with quality settings
        let video_quality = quality_to_yt_quality(request.quality);

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
            .map_err(|e| YtdlpError::DownloadError(format!("Failed to create output dir: {e}")))?;

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

/// Render a relative output path from a user template.
///
/// Supported placeholders:
/// - `{title}` / `{{ title }}` -> video title (sanitized)
/// - `{id}` / `{{ id }}` -> platform video ID (sanitized)
/// - `{ext}` / `%(ext)s` -> final container extension (`mkv`)
/// - `{{ source_custom_name/or default }}` -> source display name
/// - `{{ season_by_year__episode_by_date_and_index }}` -> `SYYYYEYYYYMMDD-III`
fn render_output_relative_path(
    template: &str,
    title: &str,
    platform_video_id: &str,
    template_data: &OutputTemplateData,
) -> PathBuf {
    let safe_title = sanitize_filename_component(title);
    let safe_id = sanitize_filename_component(platform_video_id);
    let safe_source = sanitize_filename_component(&template_data.source_name);
    let season_episode = format!(
        "S{}E{}-{:03}",
        template_data.season_year,
        template_data.episode_date.format("%Y%m%d"),
        template_data.episode_index
    );

    let mut rendered = template
        .replace("{title}", &safe_title)
        .replace("{id}", &safe_id)
        .replace("{ext}", "mkv")
        .replace("%(ext)s", "mkv")
        .replace("{{ ext }}", "mkv")
        .replace("{{ext}}", "mkv")
        .replace("{{ title }}", &safe_title)
        .replace("{{title}}", &safe_title)
        .replace("{{ id }}", &safe_id)
        .replace("{{id}}", &safe_id)
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
        );

    if rendered.trim().is_empty() {
        rendered = format!("{safe_title}-{safe_id}.mkv");
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
        segments.push(format!("{safe_title}-{safe_id}.mkv"));
    }

    let last_segment = segments
        .last_mut()
        .expect("segments is guaranteed to contain at least one segment");

    let is_mkv_extension = Path::new(last_segment)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv"));
    if !is_mkv_extension {
        last_segment.push_str(".mkv");
    }

    while last_segment.ends_with(".mkv.mkv") {
        last_segment.truncate(last_segment.len().saturating_sub(4));
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
            "Unsupported template placeholder '{{{{ {raw} }}}}'. Allowed placeholders: title, id, ext, source_custom_name/or default, season_by_year__episode_by_date_and_index"
        ));
    }

    if let Some(raw) = find_unknown_single_brace_placeholder(template) {
        return Err(format!(
            "Unsupported template placeholder '{{{raw}}}'. Allowed placeholders: title, id, ext"
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
    const ALLOWED: &[&str] = &["title", "id", "ext"];

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
        );
        assert_eq!(output, PathBuf::from("F1 overtakes-Hx4xrg6wVNI.mkv"));
    }

    #[test]
    fn test_render_output_filename_sanitizes_path_chars() {
        let output =
            render_output_relative_path("../{title}/{id}", "a/b:c*?", "id/1", &template_data());
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
        );

        assert_eq!(
            output,
            PathBuf::from("F1 Channel/S2026E20260318-007 - F1 Overtake Breakdown.mkv")
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

    #[test]
    fn test_validate_output_template_accepts_advanced_folders() {
        let template = "{{ source_custom_name/or default }}/{{ season_by_year__episode_by_date_and_index }} - {{ title }}.{{ ext }}";
        assert!(validate_output_template(template).is_ok());
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
}
