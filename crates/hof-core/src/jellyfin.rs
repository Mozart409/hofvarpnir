//! Jellyfin metadata generation for sources.
//!
//! This module generates Jellyfin-compatible metadata files:
//! - `tvshow.nfo`: XML metadata file for the show/channel
//! - `poster.jpg`: Channel avatar/thumbnail
//! - `fanart.jpg`: Channel banner (uses thumbnail as fallback)
//!
//! Files are written to the source's output directory under `completed/`.

use std::path::Path;

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr};
use reqwest::Client;
use tokio::fs;
use tracing::{debug, info, instrument, warn};

use crate::domain::source::Source;

/// Files that make up Jellyfin metadata for a TV show (`YouTube` channel).
pub const TVSHOW_NFO: &str = "tvshow.nfo";
pub const POSTER_JPG: &str = "poster.jpg";
pub const FANART_JPG: &str = "fanart.jpg";
pub const BANNER_JPG: &str = "banner.jpg";

/// Metadata needed to generate Jellyfin files.
#[derive(Debug, Clone)]
pub struct JellyfinMetadata {
    /// Display title for the show.
    pub title: String,
    /// Show description/plot.
    pub description: Option<String>,
    /// Platform-specific ID (e.g., `YouTube` channel ID).
    pub platform_id: Option<String>,
    /// Platform name (e.g., "youtube").
    pub platform: String,
    /// URL to download poster image from.
    pub poster_url: Option<String>,
    /// URL to download fanart image from.
    pub fanart_url: Option<String>,
    /// When the show was added to the library.
    pub date_added: DateTime<Utc>,
}

impl JellyfinMetadata {
    /// Create metadata from a Source.
    #[must_use]
    pub fn from_source(source: &Source, platform: &str) -> Self {
        Self {
            title: source.display_name().to_string(),
            description: source.channel_description.clone(),
            platform_id: source.channel_id.clone(),
            platform: platform.to_string(),
            poster_url: source.channel_thumbnail_url.clone(),
            fanart_url: source.channel_thumbnail_url.clone(), // Use same as poster for now
            date_added: source.created_at,
        }
    }
}

/// Check if Jellyfin metadata files exist in the given directory.
#[must_use]
pub fn metadata_files_exist(base_dir: &Path) -> MetadataStatus {
    MetadataStatus {
        nfo_exists: base_dir.join(TVSHOW_NFO).exists(),
        poster_exists: base_dir.join(POSTER_JPG).exists(),
        fanart_exists: base_dir.join(FANART_JPG).exists(),
        banner_exists: base_dir.join(BANNER_JPG).exists(),
    }
}

/// Status of metadata files in a directory.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub struct MetadataStatus {
    pub nfo_exists: bool,
    pub poster_exists: bool,
    pub fanart_exists: bool,
    pub banner_exists: bool,
}

impl MetadataStatus {
    /// Returns true if all required files exist.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.nfo_exists && self.poster_exists && self.fanart_exists
    }

    /// Returns true if any files are missing.
    #[must_use]
    pub fn has_missing(&self) -> bool {
        !self.is_complete()
    }
}

/// Generate the tvshow.nfo XML content.
#[must_use]
pub fn generate_nfo_content(metadata: &JellyfinMetadata, base_path: &Path) -> String {
    let title = xml_escape(&metadata.title);
    let description = metadata
        .description
        .as_ref()
        .map(|d| xml_escape(d))
        .unwrap_or_default();
    let date_added = metadata.date_added.format("%Y-%m-%d %H:%M:%S").to_string();

    let platform_id = metadata.platform_id.as_deref().unwrap_or("");
    let platform = &metadata.platform;

    // Build paths relative to the NFO file location
    let poster_path = base_path.join(POSTER_JPG);
    let fanart_path = base_path.join(FANART_JPG);

    let poster_str = poster_path.to_string_lossy();
    let fanart_str = fanart_path.to_string_lossy();

    format!(
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<tvshow>
  <plot>{description}</plot>
  <outline>{description}</outline>
  <lockdata>false</lockdata>
  <dateadded>{date_added}</dateadded>
  <title>{title}</title>
  <genre>YouTube</genre>
  <{platform}id>{platform_id}</{platform}id>
  <art>
    <poster>{poster_str}</poster>
    <fanart>{fanart_str}</fanart>
  </art>
  <season>-1</season>
  <episode>-1</episode>
  <uniqueid type="{platform}" default="true">{platform_id}</uniqueid>
</tvshow>
"#
    )
}

/// Escape special XML characters.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Generate all Jellyfin metadata files for a source.
///
/// This will:
/// 1. Create the tvshow.nfo file
/// 2. Download poster.jpg if a URL is available
/// 3. Download fanart.jpg if a URL is available
/// 4. Copy poster to banner.jpg as fallback
///
/// # Errors
///
/// Returns an error if the output directory cannot be created or files cannot be written.
#[instrument(skip(http_client, metadata), fields(title = %metadata.title))]
pub async fn generate_metadata(
    http_client: &Client,
    metadata: &JellyfinMetadata,
    output_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(output_dir)
        .await
        .wrap_err("Failed to create output directory")?;

    // Generate NFO file
    let nfo_path = output_dir.join(TVSHOW_NFO);
    let nfo_content = generate_nfo_content(metadata, output_dir);
    fs::write(&nfo_path, &nfo_content)
        .await
        .wrap_err("Failed to write tvshow.nfo")?;
    info!(path = %nfo_path.display(), "Generated tvshow.nfo");

    // Download poster
    let poster_path = output_dir.join(POSTER_JPG);
    if let Some(url) = &metadata.poster_url {
        if let Err(e) = download_image(http_client, url, &poster_path).await {
            warn!(error = %e, url = %url, "Failed to download poster image");
        }
    } else {
        warn!("No poster URL available - run 'Trigger Index' first to fetch channel metadata");
    }

    // Download fanart (or copy poster as fallback)
    let fanart_path = output_dir.join(FANART_JPG);
    if let Some(url) = &metadata.fanart_url {
        if let Err(e) = download_image(http_client, url, &fanart_path).await {
            warn!(error = %e, url = %url, "Failed to download fanart image");
            // Try to copy poster as fallback
            if poster_path.exists() {
                fs::copy(&poster_path, &fanart_path)
                    .await
                    .wrap_err("Failed to copy poster to fanart")?;
                debug!("Copied poster to fanart as fallback");
            }
        }
    } else if poster_path.exists() {
        fs::copy(&poster_path, &fanart_path)
            .await
            .wrap_err("Failed to copy poster to fanart")?;
        debug!("Copied poster to fanart (no fanart URL)");
    }

    // Create banner from poster as fallback
    let banner_path = output_dir.join(BANNER_JPG);
    if poster_path.exists() && !banner_path.exists() {
        fs::copy(&poster_path, &banner_path)
            .await
            .wrap_err("Failed to copy poster to banner")?;
        debug!("Copied poster to banner as fallback");
    }

    info!("Jellyfin metadata generation complete");
    Ok(())
}

/// Download an image from a URL and save it to the given path.
#[instrument(skip(http_client), fields(url = %url))]
async fn download_image(http_client: &Client, url: &str, output_path: &Path) -> Result<()> {
    debug!("Downloading image");

    let response = http_client
        .get(url)
        .send()
        .await
        .wrap_err("Failed to fetch image")?;

    if !response.status().is_success() {
        return Err(color_eyre::eyre::eyre!(
            "HTTP error {}: {}",
            response.status(),
            response.status().canonical_reason().unwrap_or("Unknown")
        ));
    }

    let bytes = response
        .bytes()
        .await
        .wrap_err("Failed to read image bytes")?;

    fs::write(output_path, &bytes)
        .await
        .wrap_err("Failed to write image file")?;

    info!(path = %output_path.display(), size = bytes.len(), "Downloaded image");
    Ok(())
}

/// Check if metadata needs to be regenerated.
///
/// Returns true if:
/// - Any required files are missing
/// - The metadata was never generated
/// - Force regeneration is requested
#[must_use]
pub fn needs_regeneration(
    base_dir: &Path,
    last_generated: Option<DateTime<Utc>>,
    force: bool,
) -> bool {
    if force {
        return true;
    }

    if last_generated.is_none() {
        return true;
    }

    let status = metadata_files_exist(base_dir);
    status.has_missing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn sample_metadata() -> JellyfinMetadata {
        JellyfinMetadata {
            title: "Test Channel".to_string(),
            description: Some("A test channel description".to_string()),
            platform_id: Some("UCxxxxxxxxxxxxxxxxx".to_string()),
            platform: "youtube".to_string(),
            poster_url: None,
            fanart_url: None,
            date_added: Utc.with_ymd_and_hms(2026, 3, 18, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn test_generate_nfo_content() {
        let metadata = sample_metadata();
        let base_path = Path::new("/data/youtube/shows/TestChannel");
        let nfo = generate_nfo_content(&metadata, base_path);

        assert!(nfo.contains("<title>Test Channel</title>"));
        assert!(nfo.contains("<plot>A test channel description</plot>"));
        assert!(nfo.contains("<youtubeid>UCxxxxxxxxxxxxxxxxx</youtubeid>"));
        assert!(nfo.contains("uniqueid type=\"youtube\""));
        assert!(nfo.contains("<dateadded>2026-03-18 12:00:00</dateadded>"));
    }

    #[test]
    fn test_generate_nfo_escapes_xml() {
        let metadata = JellyfinMetadata {
            title: "Test & Channel <Special>".to_string(),
            description: Some("Description with \"quotes\" & <tags>".to_string()),
            platform_id: Some("UC123".to_string()),
            platform: "youtube".to_string(),
            poster_url: None,
            fanart_url: None,
            date_added: Utc::now(),
        };
        let base_path = Path::new("/data");
        let nfo = generate_nfo_content(&metadata, base_path);

        assert!(nfo.contains("Test &amp; Channel &lt;Special&gt;"));
        assert!(nfo.contains("&quot;quotes&quot; &amp; &lt;tags&gt;"));
    }

    #[test]
    fn test_metadata_status() {
        let temp = TempDir::new().unwrap();
        let status = metadata_files_exist(temp.path());

        assert!(!status.nfo_exists);
        assert!(!status.poster_exists);
        assert!(!status.fanart_exists);
        assert!(status.has_missing());
        assert!(!status.is_complete());
    }

    #[test]
    fn test_metadata_status_complete() {
        let temp = TempDir::new().unwrap();

        // Create all required files
        std::fs::write(temp.path().join(TVSHOW_NFO), "test").unwrap();
        std::fs::write(temp.path().join(POSTER_JPG), "test").unwrap();
        std::fs::write(temp.path().join(FANART_JPG), "test").unwrap();

        let status = metadata_files_exist(temp.path());

        assert!(status.nfo_exists);
        assert!(status.poster_exists);
        assert!(status.fanart_exists);
        assert!(status.is_complete());
        assert!(!status.has_missing());
    }

    #[test]
    fn test_needs_regeneration() {
        let temp = TempDir::new().unwrap();

        // No files, never generated -> needs regeneration
        assert!(needs_regeneration(temp.path(), None, false));

        // Force -> needs regeneration
        assert!(needs_regeneration(temp.path(), Some(Utc::now()), true));

        // Create files
        std::fs::write(temp.path().join(TVSHOW_NFO), "test").unwrap();
        std::fs::write(temp.path().join(POSTER_JPG), "test").unwrap();
        std::fs::write(temp.path().join(FANART_JPG), "test").unwrap();

        // All files exist, was generated -> no regeneration needed
        assert!(!needs_regeneration(temp.path(), Some(Utc::now()), false));
    }

    #[tokio::test]
    async fn test_generate_metadata_creates_nfo() {
        let temp = TempDir::new().unwrap();
        let metadata = sample_metadata();
        let client = Client::new();

        generate_metadata(&client, &metadata, temp.path())
            .await
            .unwrap();

        let nfo_path = temp.path().join(TVSHOW_NFO);
        assert!(nfo_path.exists());

        let content = std::fs::read_to_string(&nfo_path).unwrap();
        assert!(content.contains("<title>Test Channel</title>"));
    }
}
