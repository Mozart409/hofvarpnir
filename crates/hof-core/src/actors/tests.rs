//! Unit tests for actor logic.
//!
//! These tests focus on pure functions and logic within actors that can be
//! tested without spawning actual actor instances or database connections.
//!
//! Note: Tests for `download_worker` URL building and file movement are in
//! `download_worker.rs` itself. This module tests additional logic.

// ============================================================================
// Download Supervisor Tests
// ============================================================================

#[cfg(test)]
mod download_supervisor_tests {
    // These constants match the values in download_supervisor.rs
    const BACKOFF_BASE_SECS: u64 = 120; // 2 minutes
    const BACKOFF_MAX_SECS: u64 = 3840; // 64 minutes

    /// Test exponential backoff calculation logic.
    #[test]
    fn test_exponential_backoff_calculation() {
        // First attempt: base delay
        let delay1 = BACKOFF_BASE_SECS;
        assert_eq!(delay1, 120);

        // Second attempt: 2x
        let delay2 = BACKOFF_BASE_SECS * 2;
        assert_eq!(delay2, 240);

        // Third attempt: 4x
        let delay3 = BACKOFF_BASE_SECS * 4;
        assert_eq!(delay3, 480);

        // Should cap at max
        let large_multiplier = 1024u64;
        let capped = (BACKOFF_BASE_SECS * large_multiplier).min(BACKOFF_MAX_SECS);
        assert_eq!(capped, BACKOFF_MAX_SECS);
    }

    /// Test retry delay calculation formula.
    #[test]
    fn test_retry_delay_formula() {
        // Formula: min(base * 2^(attempts-1), max)
        let base = BACKOFF_BASE_SECS;
        let max = BACKOFF_MAX_SECS;

        let calc_delay = |attempts: u32| -> u64 {
            if attempts == 0 {
                return 0;
            }
            let multiplier = 1u64 << (attempts - 1).min(10); // Cap at 2^10 to avoid overflow
            (base * multiplier).min(max)
        };

        assert_eq!(calc_delay(1), 120); // 120 * 1
        assert_eq!(calc_delay(2), 240); // 120 * 2
        assert_eq!(calc_delay(3), 480); // 120 * 4
        assert_eq!(calc_delay(4), 960); // 120 * 8
        assert_eq!(calc_delay(5), 1920); // 120 * 16
        assert_eq!(calc_delay(6), 3840); // 120 * 32 = max

        // Very large attempts should still cap correctly
        assert!(calc_delay(100) <= max);
    }

    #[test]
    fn test_supervisor_status_fields() {
        use super::super::download_supervisor::SupervisorStatus;

        let status = SupervisorStatus {
            active_downloads: 5,
            available_permits: 3,
            rate_limit_backoff: 2,
        };

        assert_eq!(status.active_downloads, 5);
        assert_eq!(status.available_permits, 3);
        assert_eq!(status.rate_limit_backoff, 2);
    }
}

// ============================================================================
// Source Indexer Tests
// ============================================================================

#[cfg(test)]
mod source_indexer_tests {
    use crate::ytdlp::PlaylistEntry;

    /// Helper function to check if an entry is likely a short.
    fn is_likely_short(entry: &PlaylistEntry) -> bool {
        let title_lower = entry.title.to_lowercase();
        title_lower.contains("#shorts") || title_lower.contains("#short")
    }

    #[test]
    fn test_is_likely_short_hashtag_variations() {
        // With #Shorts (capital S)
        let short1 = PlaylistEntry {
            platform_video_id: "abc123".to_string(),
            title: "Cool Moment #Shorts".to_string(),
            url: "https://youtube.com/watch?v=abc123".to_string(),
            duration_secs: Some(30),
            thumbnail_url: None,
        };
        assert!(is_likely_short(&short1));

        // With #shorts (lowercase)
        let short2 = PlaylistEntry {
            platform_video_id: "def456".to_string(),
            title: "Another moment #shorts".to_string(),
            url: "https://youtube.com/watch?v=def456".to_string(),
            duration_secs: Some(45),
            thumbnail_url: None,
        };
        assert!(is_likely_short(&short2));

        // With #short (singular)
        let short3 = PlaylistEntry {
            platform_video_id: "ghi789".to_string(),
            title: "Quick clip #short".to_string(),
            url: "https://youtube.com/watch?v=ghi789".to_string(),
            duration_secs: Some(15),
            thumbnail_url: None,
        };
        assert!(is_likely_short(&short3));
    }

    #[test]
    fn test_is_likely_short_false_positives() {
        // Regular video with 'short' in title but not as hashtag
        let regular1 = PlaylistEntry {
            platform_video_id: "xyz789".to_string(),
            title: "A Short Tutorial on Programming".to_string(),
            url: "https://youtube.com/watch?v=xyz789".to_string(),
            duration_secs: Some(600),
            thumbnail_url: None,
        };
        assert!(!is_likely_short(&regular1));

        // Regular video, no mention of shorts
        let regular2 = PlaylistEntry {
            platform_video_id: "uvw456".to_string(),
            title: "Complete Guide to Rust".to_string(),
            url: "https://youtube.com/watch?v=uvw456".to_string(),
            duration_secs: Some(3600),
            thumbnail_url: None,
        };
        assert!(!is_likely_short(&regular2));
    }

    #[test]
    fn test_indexing_result_defaults() {
        use super::super::source_indexer::IndexingResult;
        use ulid::Ulid;

        let result = IndexingResult {
            source_id: Ulid::new(),
            new_videos: 0,
            existing_videos: 0,
            filtered_out: 0,
            filtered_before_cutoff: 0,
            filtered_shorts: 0,
            filtered_livestreams: 0,
            filtered_unavailable: 0,
            filtered_private: 0,
            filtered_other: 0,
            errors: Vec::new(),
        };

        assert_eq!(result.new_videos, 0);
        assert_eq!(result.existing_videos, 0);
        assert_eq!(result.filtered_out, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_indexing_result_with_data() {
        use super::super::source_indexer::IndexingResult;
        use ulid::Ulid;

        let result = IndexingResult {
            source_id: Ulid::new(),
            new_videos: 10,
            existing_videos: 50,
            filtered_out: 5,
            filtered_before_cutoff: 1,
            filtered_shorts: 1,
            filtered_livestreams: 1,
            filtered_unavailable: 1,
            filtered_private: 1,
            filtered_other: 0,
            errors: vec!["Rate limited".to_string()],
        };

        assert_eq!(result.new_videos, 10);
        assert_eq!(result.existing_videos, 50);
        assert_eq!(result.filtered_out, 5);
        assert_eq!(result.filtered_before_cutoff, 1);
        assert_eq!(result.filtered_shorts, 1);
        assert_eq!(result.filtered_livestreams, 1);
        assert_eq!(result.filtered_unavailable, 1);
        assert_eq!(result.filtered_private, 1);
        assert_eq!(result.filtered_other, 0);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0], "Rate limited");
    }
}

// ============================================================================
// Scheduler Tests
// ============================================================================

#[cfg(test)]
mod scheduler_tests {
    use std::time::Duration;

    /// Minimum interval constant from scheduler (5 minutes)
    const MIN_INDEX_INTERVAL_SECS: u64 = 300;
    /// Default check interval (1 minute)
    const DEFAULT_CHECK_INTERVAL_SECS: u64 = 60;

    #[test]
    fn test_duration_from_secs() {
        let interval = Duration::from_secs(MIN_INDEX_INTERVAL_SECS);
        assert_eq!(interval.as_secs(), 300);

        let check = Duration::from_secs(DEFAULT_CHECK_INTERVAL_SECS);
        assert_eq!(check.as_secs(), 60);
    }

    #[test]
    fn test_scheduler_status_fields() {
        use super::super::scheduler::SchedulerStatus;

        let status = SchedulerStatus {
            running: true,
            active_indexers: 3,
            check_interval_secs: 60,
        };

        assert!(status.running);
        assert_eq!(status.active_indexers, 3);
        assert_eq!(status.check_interval_secs, 60);
    }
}

// ============================================================================
// Cleanup Actor Tests
// ============================================================================

#[cfg(test)]
mod cleanup_tests {
    use std::path::Path;
    use std::time::Duration;

    /// Default cleanup interval (15 minutes)
    const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 60 * 15;

    #[test]
    fn test_cleanup_interval_duration() {
        let interval = Duration::from_secs(DEFAULT_CLEANUP_INTERVAL_SECS);
        assert_eq!(interval.as_secs(), 900); // 15 minutes
    }

    #[test]
    fn test_is_ytdlp_temp_file() {
        // .part files are temp files
        assert!(is_ytdlp_temp_file(Path::new("/downloads/video.mp4.part")));

        // .ytdl files are temp files
        assert!(is_ytdlp_temp_file(Path::new("/downloads/video.ytdl")));

        // temp_audio_* files are temp merge files
        assert!(is_ytdlp_temp_file(Path::new(
            "/downloads/temp_audio_abc123.m4a"
        )));

        // temp_video_* files are temp merge files
        assert!(is_ytdlp_temp_file(Path::new(
            "/downloads/temp_video_abc123.mp4"
        )));

        // Regular video files are not temp files
        assert!(!is_ytdlp_temp_file(Path::new("/downloads/video.mp4")));

        // Regular subtitle files are not temp files
        assert!(!is_ytdlp_temp_file(Path::new("/downloads/video.en.srt")));

        // NFO files are not temp files
        assert!(!is_ytdlp_temp_file(Path::new("/downloads/video.nfo")));
    }

    #[test]
    fn test_cleanup_result_defaults() {
        use super::super::cleanup::CleanupResult;

        let result = CleanupResult::default();

        assert_eq!(result.retention_cleaned, 0);
        assert_eq!(result.quota_cleaned, 0);
        assert_eq!(result.temp_files_cleaned, 0);
        assert_eq!(result.bytes_freed, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_cleanup_result_with_data() {
        use super::super::cleanup::CleanupResult;

        let result = CleanupResult {
            retention_cleaned: 5,
            quota_cleaned: 3,
            temp_files_cleaned: 10,
            bytes_freed: 5_000_000_000,
            errors: vec!["Failed to delete file".to_string()],
        };

        assert_eq!(result.retention_cleaned, 5);
        assert_eq!(result.quota_cleaned, 3);
        assert_eq!(result.temp_files_cleaned, 10);
        assert_eq!(result.bytes_freed, 5_000_000_000);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_cleanup_status_fields() {
        use super::super::cleanup::CleanupStatus;

        let status = CleanupStatus {
            running: true,
            global_retention_days: Some(30),
            cleanup_interval_secs: 900,
            last_run_at: None,
        };

        assert!(status.running);
        assert_eq!(status.global_retention_days, Some(30));
        assert_eq!(status.cleanup_interval_secs, 900);
        assert!(status.last_run_at.is_none());
    }

    #[test]
    fn test_cleanup_status_without_retention() {
        use super::super::cleanup::CleanupStatus;

        let status = CleanupStatus {
            running: false,
            global_retention_days: None,
            cleanup_interval_secs: 1800,
            last_run_at: Some(chrono::Utc::now()),
        };

        assert!(!status.running);
        assert!(status.global_retention_days.is_none());
        assert_eq!(status.cleanup_interval_secs, 1800);
        assert!(status.last_run_at.is_some());
    }

    /// Helper function to check if a path is a yt-dlp temp file.
    fn is_ytdlp_temp_file(path: &Path) -> bool {
        let is_part = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("part"));
        let is_ytdl = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ytdl"));
        let is_temp_merge = path.file_name().is_some_and(|name| {
            let n = name.to_string_lossy();
            n.starts_with("temp_audio_") || n.starts_with("temp_video_")
        });

        is_part || is_ytdl || is_temp_merge
    }
}

// ============================================================================
// Video Filter Tests
// ============================================================================

#[cfg(test)]
mod video_filter_tests {
    use chrono::{NaiveDate, Utc};

    use crate::domain::profile::{OutputPreset, Profile, Quality};
    use crate::domain::source::{EntryOrder, Source, SourceType};
    use crate::ytdlp::VideoMetadata;
    use ulid::Ulid;

    fn test_profile(include_shorts: bool, include_livestreams: bool) -> Profile {
        Profile {
            id: Ulid::new(),
            user_id: Ulid::new(),
            name: "Test Profile".to_string(),
            quality: Quality::Q1080p,
            output_preset: OutputPreset::Browser,
            naming_template: "{title}.{ext}".to_string(),
            output_dir: "/downloads".to_string(),
            include_livestreams,
            include_shorts,
            storage_quota_bytes: 100_000_000_000,
            retention_days: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_source(cutoff: NaiveDate) -> Source {
        Source {
            id: Ulid::new(),
            profile_id: Ulid::new(),
            url: "https://youtube.com/@test".to_string(),
            source_type: SourceType::Channel,
            custom_name: None,
            enabled: true,
            index_frequency_secs: 3600,
            cutoff_date: cutoff,
            retention_days: None,
            entry_order: EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: None,
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        }
    }

    fn test_video_metadata(
        published_at: Option<chrono::DateTime<Utc>>,
        is_short: bool,
        is_live: bool,
        was_live: bool,
    ) -> VideoMetadata {
        VideoMetadata {
            platform: "youtube".to_string(),
            platform_video_id: "test123".to_string(),
            title: if is_short {
                "Test #Shorts".to_string()
            } else {
                "Test Video".to_string()
            },
            description: None,
            duration_secs: if is_short { Some(30) } else { Some(600) },
            published_at,
            thumbnail_url: None,
            is_live,
            was_live,
            media_type: if is_short {
                Some("short".to_string())
            } else {
                None
            },
        }
    }

    /// Simulates the filter logic from `SourceIndexerActor`.
    fn should_filter_video(
        profile: &Profile,
        source: &Source,
        metadata: &VideoMetadata,
    ) -> Option<String> {
        // Check shorts
        if !profile.include_shorts && metadata.is_short() {
            return Some("short video excluded".to_string());
        }

        // Check livestreams
        if !profile.include_livestreams && (metadata.is_live || metadata.was_live) {
            return Some("livestream excluded".to_string());
        }

        // Check cutoff date
        if let Some(published) = metadata.published_at {
            let published_date = published.date_naive();
            if published_date < source.cutoff_date {
                return Some(format!(
                    "published {} before cutoff {}",
                    published_date, source.cutoff_date
                ));
            }
        }

        None // Video should be included
    }

    #[test]
    fn test_filter_shorts_when_disabled() {
        let profile = test_profile(false, true);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let metadata = test_video_metadata(Some(Utc::now()), true, false, false);

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_some());
        assert!(result.unwrap().contains("short"));
    }

    #[test]
    fn test_allow_shorts_when_enabled() {
        let profile = test_profile(true, true);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let metadata = test_video_metadata(Some(Utc::now()), true, false, false);

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_none()); // Should not be filtered
    }

    #[test]
    fn test_filter_livestream_when_disabled() {
        let profile = test_profile(true, false);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let metadata = test_video_metadata(Some(Utc::now()), false, false, true); // was_live = true

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_some());
        assert!(result.unwrap().contains("livestream"));
    }

    #[test]
    fn test_allow_livestream_when_enabled() {
        let profile = test_profile(true, true);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let metadata = test_video_metadata(Some(Utc::now()), false, false, true);

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_none()); // Should not be filtered
    }

    #[test]
    fn test_filter_before_cutoff() {
        let profile = test_profile(true, true);
        let source = test_source(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());

        // Video published in 2023 (before 2024 cutoff)
        let old_date = chrono::DateTime::parse_from_rfc3339("2023-06-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metadata = test_video_metadata(Some(old_date), false, false, false);

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_some());
        assert!(result.unwrap().contains("cutoff"));
    }

    #[test]
    fn test_allow_after_cutoff() {
        let profile = test_profile(true, true);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());

        // Video published in 2024 (after 2020 cutoff)
        let new_date = chrono::DateTime::parse_from_rfc3339("2024-06-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metadata = test_video_metadata(Some(new_date), false, false, false);

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_none()); // Should not be filtered
    }

    #[test]
    fn test_allow_video_without_publish_date() {
        let profile = test_profile(true, true);
        let source = test_source(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let metadata = test_video_metadata(None, false, false, false);

        // No publish date = can't filter by cutoff
        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_none());
    }

    #[test]
    fn test_filter_live_stream_currently_live() {
        let profile = test_profile(true, false);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let metadata = test_video_metadata(Some(Utc::now()), false, true, false); // is_live = true

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_some());
        assert!(result.unwrap().contains("livestream"));
    }

    #[test]
    fn test_multiple_filters_shorts_takes_precedence() {
        // When both shorts and livestream would filter, shorts filter runs first
        let profile = test_profile(false, false);
        let source = test_source(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let metadata = test_video_metadata(Some(Utc::now()), true, false, true);

        let result = should_filter_video(&profile, &source, &metadata);
        assert!(result.is_some());
        // Shorts filter runs first
        assert!(result.unwrap().contains("short"));
    }
}

// ============================================================================
// Domain Type Tests
// ============================================================================

#[cfg(test)]
mod domain_tests {
    use chrono::{NaiveDate, Utc};
    use ulid::Ulid;

    use crate::domain::profile::Quality;
    use crate::domain::source::{EntryOrder, Source, SourceType};
    use crate::domain::video::VideoStatus;

    #[test]
    fn test_video_status_equality() {
        assert_eq!(VideoStatus::Pending, VideoStatus::Pending);
        assert_ne!(VideoStatus::Pending, VideoStatus::Completed);
        assert_ne!(VideoStatus::Failed, VideoStatus::PermanentlyFailed);
    }

    #[test]
    fn test_quality_variants_count() {
        // Ensure we have all expected quality variants
        let variants = [
            Quality::Best,
            Quality::Q4320p,
            Quality::Q2160p,
            Quality::Q1440p,
            Quality::Q1080p,
            Quality::Q720p,
            Quality::Q480p,
            Quality::AudioOnly,
        ];

        assert_eq!(variants.len(), 8);
    }

    #[test]
    fn test_source_display_name_custom() {
        let source = Source {
            id: Ulid::new(),
            profile_id: Ulid::new(),
            url: "https://youtube.com/@channel".to_string(),
            source_type: SourceType::Channel,
            custom_name: Some("My Custom Name".to_string()),
            enabled: true,
            index_frequency_secs: 3600,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            retention_days: None,
            entry_order: EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: Some("Channel Title".to_string()),
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        };

        // Custom name takes precedence
        assert_eq!(source.display_name(), "My Custom Name");
    }

    #[test]
    fn test_source_display_name_channel_title() {
        let source = Source {
            id: Ulid::new(),
            profile_id: Ulid::new(),
            url: "https://youtube.com/@channel".to_string(),
            source_type: SourceType::Channel,
            custom_name: None, // No custom name
            enabled: true,
            index_frequency_secs: 3600,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            retention_days: None,
            entry_order: EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: Some("Channel Title".to_string()),
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        };

        // Falls back to channel title
        assert_eq!(source.display_name(), "Channel Title");
    }

    #[test]
    fn test_source_display_name_url_fallback() {
        let source = Source {
            id: Ulid::new(),
            profile_id: Ulid::new(),
            url: "https://youtube.com/@channel".to_string(),
            source_type: SourceType::Channel,
            custom_name: None,
            enabled: true,
            index_frequency_secs: 3600,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            retention_days: None,
            entry_order: EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: None, // No channel title either
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        };

        // Falls back to URL
        assert_eq!(source.display_name(), "https://youtube.com/@channel");
    }

    #[test]
    fn test_video_status_all_variants() {
        // Ensure we can create all status variants
        let statuses = [
            VideoStatus::Pending,
            VideoStatus::Downloading,
            VideoStatus::Completed,
            VideoStatus::Failed,
            VideoStatus::Skipped,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
        ];

        assert_eq!(statuses.len(), 7);
    }

    #[test]
    fn test_source_type_variants() {
        assert!(matches!(SourceType::Channel, SourceType::Channel));
        assert!(matches!(SourceType::Playlist, SourceType::Playlist));
    }

    #[test]
    fn test_source_completed_dir() {
        let source = Source {
            id: Ulid::new(),
            profile_id: Ulid::new(),
            url: "https://youtube.com/@channel".to_string(),
            source_type: SourceType::Channel,
            custom_name: Some("My Channel".to_string()),
            enabled: true,
            index_frequency_secs: 3600,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            retention_days: None,
            entry_order: EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: None,
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        };

        let completed_dir = source.completed_dir("/downloads");
        assert!(completed_dir.to_string_lossy().contains("completed"));
        assert!(completed_dir.to_string_lossy().contains("My Channel"));
    }
}

// ============================================================================
// Jellyfin Metadata Actor Tests
// ============================================================================

#[cfg(test)]
mod jellyfin_metadata_tests {
    use super::super::jellyfin_metadata::{JellyfinMetadataStatus, SourceMetadataResult};

    #[test]
    fn test_metadata_status_fields() {
        let status = JellyfinMetadataStatus {
            is_running: true,
            last_check_at: None,
            next_check_at: None,
        };

        assert!(status.is_running);
        assert!(status.last_check_at.is_none());
        assert!(status.next_check_at.is_none());
    }

    #[test]
    fn test_metadata_status_with_times() {
        use chrono::Utc;

        let now = Utc::now();
        let status = JellyfinMetadataStatus {
            is_running: false,
            last_check_at: Some(now),
            next_check_at: Some(now + chrono::Duration::hours(24)),
        };

        assert!(!status.is_running);
        assert!(status.last_check_at.is_some());
        assert!(status.next_check_at.is_some());
    }

    #[test]
    fn test_source_metadata_result_success() {
        let result = SourceMetadataResult {
            success: true,
            error: None,
        };

        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_source_metadata_result_failure() {
        let result = SourceMetadataResult {
            success: false,
            error: Some("Failed to download thumbnail".to_string()),
        };

        assert!(!result.success);
        assert_eq!(
            result.error,
            Some("Failed to download thumbnail".to_string())
        );
    }
}
