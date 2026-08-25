//! Integration tests for API routes.
//!
//! These tests focus on request/response validation, serialization,
//! ULID parsing, and error handling for API types.

// ============================================================================
// Not Found Tests
// ============================================================================

#[cfg(test)]
mod not_found_tests {
    use crate::auth::ApiErrorResponse;

    #[test]
    fn test_api_error_response_schema() {
        let response = ApiErrorResponse {
            error: "not_found".to_string(),
            message: "The requested API endpoint does not exist".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"error\":\"not_found\""));
        assert!(json.contains("The requested API endpoint does not exist"));
    }

    #[test]
    fn test_api_error_response_deserialization() {
        let json = r#"{"error":"not_found","message":"Resource not found"}"#;
        let response: ApiErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.error, "not_found");
        assert_eq!(response.message, "Resource not found");
    }
}

// ============================================================================
// Profile Route Tests
// ============================================================================

#[cfg(test)]
mod profile_tests {
    use crate::routes::profiles::{
        CreateProfileRequest, ErrorResponse, ProfileResponse, UpdateProfileRequest,
    };
    use hof_core::domain::profile::{OutputPreset, Quality};

    #[test]
    fn test_profile_response_from_profile() {
        use chrono::Utc;
        use hof_core::domain::profile::Profile;
        use ulid::Ulid;

        let profile = Profile {
            id: Ulid::generate(),
            user_id: Ulid::generate(),
            name: "Test Profile".to_string(),
            quality: Quality::Q1080p,
            output_preset: OutputPreset::Browser,
            naming_template: "{title}-{id}.{ext}".to_string(),
            output_dir: "/downloads".to_string(),
            include_livestreams: false,
            include_shorts: false,
            storage_quota_bytes: 100_000_000_000,
            retention_days: Some(30),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let response: ProfileResponse = profile.clone().into();

        assert_eq!(response.id, profile.id.to_string());
        assert_eq!(response.user_id, profile.user_id.to_string());
        assert_eq!(response.name, "Test Profile");
        assert!(matches!(response.quality, Quality::Q1080p));
        assert!(matches!(response.output_preset, OutputPreset::Browser));
        assert_eq!(response.naming_template, "{title}-{id}.{ext}");
        assert_eq!(response.output_dir, "/downloads");
        assert!(!response.include_livestreams);
        assert!(!response.include_shorts);
        assert_eq!(response.storage_quota_bytes, 100_000_000_000);
        assert_eq!(response.retention_days, Some(30));
    }

    #[test]
    fn test_create_profile_request_default_values() {
        let json = r#"{
            "user_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "My Profile",
            "quality": "Q1080p",
            "naming_template": "{title}.{ext}",
            "output_dir": "/downloads"
        }"#;

        let req: CreateProfileRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.user_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(req.name, "My Profile");
        assert!(!req.include_livestreams); // default false
        assert!(!req.include_shorts); // default false
        assert!(req.output_preset.is_none());
        // Default quota is 100GB
        assert_eq!(req.storage_quota_bytes, 100_000_000_000);
        assert!(req.retention_days.is_none());
    }

    #[test]
    fn test_create_profile_request_with_all_fields() {
        let json = r#"{
            "user_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "Full Profile",
            "quality": "Best",
            "output_preset": "Tv",
            "naming_template": "{title}-{id}.{ext}",
            "output_dir": "/media/videos",
            "include_livestreams": true,
            "include_shorts": true,
            "storage_quota_bytes": 500000000000,
            "retention_days": 90
        }"#;

        let req: CreateProfileRequest = serde_json::from_str(json).unwrap();
        assert!(req.include_livestreams);
        assert!(req.include_shorts);
        assert!(matches!(req.output_preset, Some(OutputPreset::Tv)));
        assert_eq!(req.storage_quota_bytes, 500_000_000_000);
        assert_eq!(req.retention_days, Some(90));
    }

    #[test]
    fn test_update_profile_request_partial() {
        let json = r#"{"name": "Updated Name"}"#;

        let req: UpdateProfileRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, Some("Updated Name".to_string()));
        assert!(req.quality.is_none());
        assert!(req.output_preset.is_none());
        assert!(req.naming_template.is_none());
        assert!(req.output_dir.is_none());
        assert!(req.include_livestreams.is_none());
        assert!(req.include_shorts.is_none());
        assert!(req.storage_quota_bytes.is_none());
        // retention_days is Option<Option<i32>>: None = field not present
        assert!(req.retention_days.is_none());
    }

    #[test]
    fn test_update_profile_request_null_retention() {
        // When retention_days is explicitly null, it should become Some(None)
        let json = r#"{"retention_days": null}"#;

        let req: UpdateProfileRequest = serde_json::from_str(json).unwrap();
        // Some(None) = field present but null (clear the value)
        assert_eq!(req.retention_days, Some(None));
    }

    #[test]
    fn test_quality_enum_serialization() {
        // All quality variants should serialize correctly
        assert_eq!(serde_json::to_string(&Quality::Best).unwrap(), "\"Best\"");
        assert_eq!(
            serde_json::to_string(&Quality::Q4320p).unwrap(),
            "\"Q4320p\""
        );
        assert_eq!(
            serde_json::to_string(&Quality::Q2160p).unwrap(),
            "\"Q2160p\""
        );
        assert_eq!(
            serde_json::to_string(&Quality::Q1440p).unwrap(),
            "\"Q1440p\""
        );
        assert_eq!(
            serde_json::to_string(&Quality::Q1080p).unwrap(),
            "\"Q1080p\""
        );
        assert_eq!(serde_json::to_string(&Quality::Q720p).unwrap(), "\"Q720p\"");
        assert_eq!(serde_json::to_string(&Quality::Q480p).unwrap(), "\"Q480p\"");
        assert_eq!(
            serde_json::to_string(&Quality::AudioOnly).unwrap(),
            "\"AudioOnly\""
        );
    }

    #[test]
    fn test_error_response_serialization() {
        let error = ErrorResponse {
            error: "Invalid profile ID format".to_string(),
        };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("Invalid profile ID format"));
    }
}

// ============================================================================
// Source Route Tests
// ============================================================================

#[cfg(test)]
mod source_tests {
    use crate::routes::sources::{
        CreateSourceRequest, IndexTriggerResponse, SourceResponse, UpdateSourceRequest,
    };
    use hof_core::domain::source::SourceType;

    #[test]
    fn test_source_response_from_source() {
        use chrono::{NaiveDate, Utc};
        use hof_core::domain::source::{EntryOrder, Source};
        use ulid::Ulid;

        let source = Source {
            id: Ulid::generate(),
            profile_id: Ulid::generate(),
            url: "https://youtube.com/@channel".to_string(),
            source_type: SourceType::Channel,
            custom_name: Some("My Channel".to_string()),
            enabled: true,
            exclude_from_cleanup: false,
            index_frequency_secs: 3600,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            retention_days: Some(60),
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

        let response: SourceResponse = source.clone().into();

        assert_eq!(response.id, source.id.to_string());
        assert_eq!(response.profile_id, source.profile_id.to_string());
        assert_eq!(response.url, "https://youtube.com/@channel");
        assert!(matches!(response.source_type, SourceType::Channel));
        assert_eq!(response.custom_name, Some("My Channel".to_string()));
        assert!(response.enabled);
        assert_eq!(response.index_frequency_secs, 3600);
        assert_eq!(response.cutoff_date, "2024-01-01");
        assert_eq!(response.retention_days, Some(60));
        assert!(matches!(response.entry_order, EntryOrder::Unknown));
        assert!(response.last_indexed_at.is_none());
    }

    #[test]
    fn test_create_source_request_default_frequency() {
        let json = r#"{
            "profile_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "url": "https://youtube.com/@channel",
            "source_type": "Channel",
            "cutoff_date": "2024-01-01"
        }"#;

        let req: CreateSourceRequest = serde_json::from_str(json).unwrap();
        // Default frequency is 1 hour (3600 seconds)
        assert_eq!(req.index_frequency_secs, 3600);
        assert!(req.custom_name.is_none());
        assert!(req.retention_days.is_none());
    }

    #[test]
    fn test_create_source_request_playlist() {
        let json = r#"{
            "profile_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "url": "https://youtube.com/playlist?list=PLxxxxxx",
            "source_type": "Playlist",
            "custom_name": "My Playlist",
            "index_frequency_secs": 7200,
            "cutoff_date": "2023-06-15",
            "retention_days": 30
        }"#;

        let req: CreateSourceRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req.source_type, SourceType::Playlist));
        assert_eq!(req.custom_name, Some("My Playlist".to_string()));
        assert_eq!(req.index_frequency_secs, 7200);
        assert_eq!(req.cutoff_date, "2023-06-15");
        assert_eq!(req.retention_days, Some(30));
    }

    #[test]
    fn test_update_source_request_partial() {
        let json = r#"{"url": "https://youtube.com/@newchannel"}"#;

        let req: UpdateSourceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.url, Some("https://youtube.com/@newchannel".to_string()));
        assert!(req.source_type.is_none());
        assert!(req.custom_name.is_none());
        assert!(req.index_frequency_secs.is_none());
        assert!(req.cutoff_date.is_none());
        assert!(req.retention_days.is_none());
    }

    #[test]
    fn test_update_source_request_clear_custom_name() {
        let json = r#"{"custom_name": null}"#;

        let req: UpdateSourceRequest = serde_json::from_str(json).unwrap();
        // Some(None) = field present but null (clear the value)
        assert_eq!(req.custom_name, Some(None));
    }

    #[test]
    fn test_source_type_serialization() {
        assert_eq!(
            serde_json::to_string(&SourceType::Channel).unwrap(),
            "\"Channel\""
        );
        assert_eq!(
            serde_json::to_string(&SourceType::Playlist).unwrap(),
            "\"Playlist\""
        );
    }

    #[test]
    fn test_index_trigger_response_schema() {
        let response = IndexTriggerResponse {
            message: "Indexing started".to_string(),
            source_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Indexing started"));
        assert!(json.contains("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    }
}

// ============================================================================
// Download Route Tests
// ============================================================================

#[cfg(test)]
mod download_tests {
    use crate::routes::downloads::{
        BulkRetryResponse, CancelResponse, ListDownloadsQuery, ProgressEvent, RetryResponse,
        VideoResponse,
    };
    use hof_core::domain::video::VideoStatus;

    #[test]
    fn test_video_response_from_video() {
        use chrono::Utc;
        use hof_core::domain::video::Video;
        use ulid::Ulid;

        let video = Video {
            id: Ulid::generate(),
            platform: "youtube".to_string(),
            platform_video_id: "dQw4w9WgXcQ".to_string(),
            title: "Test Video".to_string(),
            description: Some("A test video description".to_string()),
            duration_secs: Some(215),
            published_at: Some(Utc::now()),
            thumbnail_url: Some("https://example.com/thumb.jpg".to_string()),
            status: VideoStatus::Completed,
            attempts: 1,
            next_retry: None,
            last_error: None,
            file_path: Some("/downloads/test.mp4".to_string()),
            file_size_bytes: Some(150_000_000),
            video_height: Some(1440),
            video_codec: Some("av01.0.12M.08".to_string()),
            downloaded_at: Some(Utc::now()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let response: VideoResponse = video.clone().into();

        assert_eq!(response.id, video.id.to_string());
        assert_eq!(response.platform, "youtube");
        assert_eq!(response.platform_video_id, "dQw4w9WgXcQ");
        assert_eq!(response.title, "Test Video");
        assert_eq!(
            response.description,
            Some("A test video description".to_string())
        );
        assert_eq!(response.duration_secs, Some(215));
        assert!(matches!(response.status, VideoStatus::Completed));
        assert_eq!(response.attempts, 1);
        assert!(response.last_error_code.is_none());
        assert_eq!(response.file_path, Some("/downloads/test.mp4".to_string()));
        assert_eq!(response.file_size_bytes, Some(150_000_000));
    }

    #[test]
    fn test_video_response_extracts_machine_error_code() {
        use chrono::Utc;
        use hof_core::domain::video::Video;
        use ulid::Ulid;

        let video = Video {
            id: Ulid::generate(),
            platform: "youtube".to_string(),
            platform_video_id: "dQw4w9WgXcQ".to_string(),
            title: "Test Video".to_string(),
            description: None,
            duration_secs: None,
            published_at: None,
            thumbnail_url: None,
            status: VideoStatus::Failed,
            attempts: 2,
            next_retry: Some(Utc::now()),
            last_error: Some(
                "[DOWNLOAD_FORMAT_UNAVAILABLE] Failed to download video: ...".to_string(),
            ),
            file_path: None,
            file_size_bytes: None,
            video_height: None,
            video_codec: None,
            downloaded_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let response: VideoResponse = video.into();

        assert_eq!(
            response.last_error_code,
            Some("DOWNLOAD_FORMAT_UNAVAILABLE".to_string())
        );
    }

    #[test]
    fn test_video_status_serialization() {
        assert_eq!(
            serde_json::to_string(&VideoStatus::Pending).unwrap(),
            "\"Pending\""
        );
        assert_eq!(
            serde_json::to_string(&VideoStatus::Downloading).unwrap(),
            "\"Downloading\""
        );
        assert_eq!(
            serde_json::to_string(&VideoStatus::Completed).unwrap(),
            "\"Completed\""
        );
        assert_eq!(
            serde_json::to_string(&VideoStatus::Failed).unwrap(),
            "\"Failed\""
        );
        assert_eq!(
            serde_json::to_string(&VideoStatus::Skipped).unwrap(),
            "\"Skipped\""
        );
        assert_eq!(
            serde_json::to_string(&VideoStatus::Cleaned).unwrap(),
            "\"Cleaned\""
        );
        assert_eq!(
            serde_json::to_string(&VideoStatus::PermanentlyFailed).unwrap(),
            "\"PermanentlyFailed\""
        );
    }

    #[test]
    fn test_video_status_deserialization() {
        assert!(matches!(
            serde_json::from_str::<VideoStatus>("\"Pending\"").unwrap(),
            VideoStatus::Pending
        ));
        assert!(matches!(
            serde_json::from_str::<VideoStatus>("\"Failed\"").unwrap(),
            VideoStatus::Failed
        ));
    }

    #[test]
    fn test_list_downloads_query_deserialization() {
        // Empty query
        let query: ListDownloadsQuery = serde_qs::from_str("").unwrap();
        assert!(query.status.is_none());
        assert!(query.source_id.is_none());

        // With status filter
        let query: ListDownloadsQuery = serde_qs::from_str("status=Pending").unwrap();
        assert!(matches!(query.status, Some(VideoStatus::Pending)));

        // With source_id filter
        let query: ListDownloadsQuery =
            serde_qs::from_str("source_id=01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
        assert_eq!(
            query.source_id,
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string())
        );
    }

    #[test]
    fn test_retry_response_schema() {
        let response = RetryResponse {
            message: "Retry enqueued".to_string(),
            video_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Retry enqueued"));
        assert!(json.contains("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    }

    #[test]
    fn test_bulk_retry_response_schema() {
        let response = BulkRetryResponse {
            message: "Retrying 3 downloads".to_string(),
            retried_count: 3,
            video_ids: vec![
                "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
                "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_string(),
            ],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"retried_count\":3"));
        assert!(json.contains("video_ids"));
    }

    #[test]
    fn test_cancel_response_schema() {
        let response = CancelResponse {
            message: "Download cancelled".to_string(),
            video_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Download cancelled"));
    }

    #[test]
    fn test_progress_event_schema() {
        let event = ProgressEvent {
            video_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            platform_video_id: "dQw4w9WgXcQ".to_string(),
            percent: 45.5,
            speed: Some("5.2MiB/s".to_string()),
            eta: Some("00:02:30".to_string()),
            downloaded_bytes: Some(50_000_000),
            total_bytes: Some(110_000_000),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"percent\":45.5"));
        assert!(json.contains("\"speed\":\"5.2MiB/s\""));
        assert!(json.contains("\"eta\":\"00:02:30\""));
    }
}

// ============================================================================
// Health Route Tests
// ============================================================================

#[cfg(test)]
mod health_tests {
    use crate::routes::health::{ActorsHealth, ComponentHealth, HealthResponse, HealthStatus};

    #[test]
    fn test_health_response_serialization() {
        let response = HealthResponse {
            status: HealthStatus::Healthy,
            database: ComponentHealth {
                healthy: true,
                message: None,
            },
            ytdlp: ComponentHealth {
                healthy: true,
                message: Some("yt-dlp 2024.01.01".to_string()),
            },
            actors: ActorsHealth {
                healthy: true,
                supervisor: true,
                scheduler: true,
                cleanup: true,
                jellyfin_metadata: true,
            },
            issues: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"status\":\"healthy\""));
        assert!(json.contains("\"healthy\":true"));
    }

    #[test]
    fn test_health_status_serialization() {
        assert_eq!(
            serde_json::to_string(&HealthStatus::Healthy).unwrap(),
            "\"healthy\""
        );
        assert_eq!(
            serde_json::to_string(&HealthStatus::Degraded).unwrap(),
            "\"degraded\""
        );
        assert_eq!(
            serde_json::to_string(&HealthStatus::Unhealthy).unwrap(),
            "\"unhealthy\""
        );
    }

    #[test]
    fn test_component_health_with_error() {
        let health = ComponentHealth {
            healthy: false,
            message: Some("Connection refused".to_string()),
        };

        let json = serde_json::to_string(&health).unwrap();
        assert!(json.contains("\"healthy\":false"));
        assert!(json.contains("Connection refused"));
    }

    #[test]
    fn test_component_health_without_message() {
        let health = ComponentHealth {
            healthy: true,
            message: None,
        };

        let json = serde_json::to_string(&health).unwrap();
        assert!(json.contains("\"healthy\":true"));
    }
}

// ============================================================================
// System Route Tests
// ============================================================================

#[cfg(test)]
mod system_tests {
    use chrono::Utc;

    use crate::routes::system::{
        CleanupResultResponse, CleanupStatusResponse, CleanupTriggerResponse,
        DownloadsStatusResponse, SchedulerStatusResponse, StatisticsResponse, SystemStatusResponse,
    };

    #[test]
    fn test_system_status_response_schema() {
        let response = SystemStatusResponse {
            scheduler: SchedulerStatusResponse {
                running: true,
                active_indexers: 2,
                check_interval_secs: 60,
            },
            downloads: DownloadsStatusResponse {
                active_downloads: 3,
                available_permits: 2,
                rate_limit_backoff: 0,
            },
            cleanup: CleanupStatusResponse {
                running: true,
                global_retention_days: Some(30),
                cleanup_interval_secs: 900,
                last_run_at: None,
            },
            statistics: StatisticsResponse {
                total_videos: 168,
                pending_downloads: 10,
                downloading: 3,
                completed: 150,
                failed: 5,
                permanently_failed: 0,
                skipped: 0,
                cleaned: 0,
            },
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"running\":true"));
        assert!(json.contains("\"active_indexers\":2"));
        assert!(json.contains("\"active_downloads\":3"));
        assert!(json.contains("\"pending_downloads\":10"));
        assert!(json.contains("\"completed\":150"));
    }

    #[test]
    fn test_cleanup_trigger_response_schema() {
        let response = CleanupTriggerResponse {
            message: "Cleanup completed".to_string(),
            result: CleanupResultResponse {
                retention_cleaned: 5,
                quota_cleaned: 2,
                temp_files_cleaned: 10,
                bytes_freed: 5_000_000_000,
                errors: vec![],
            },
        };

        assert_eq!(response.message, "Cleanup completed");
        assert_eq!(response.result.retention_cleaned, 5);
    }

    #[test]
    fn test_cleanup_result_response_schema() {
        let response = CleanupResultResponse {
            retention_cleaned: 5,
            quota_cleaned: 2,
            temp_files_cleaned: 10,
            bytes_freed: 5_000_000_000,
            errors: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"retention_cleaned\":5"));
        assert!(json.contains("\"bytes_freed\":5000000000"));
    }
}

// ============================================================================
// ULID Parsing Tests
// ============================================================================

#[cfg(test)]
mod ulid_tests {
    use ulid::Ulid;

    #[test]
    fn test_valid_ulid_parsing() {
        let valid_ulid = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert!(Ulid::from_string(valid_ulid).is_ok());
    }

    #[test]
    fn test_invalid_ulid_parsing() {
        // Too short
        assert!(Ulid::from_string("01ARZ3NDEK").is_err());

        // Invalid characters
        assert!(Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FA!").is_err());

        // Too long
        assert!(Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAVX").is_err());

        // Empty
        assert!(Ulid::from_string("").is_err());
    }

    #[test]
    fn test_ulid_roundtrip() {
        let original = Ulid::generate();
        let string = original.to_string();
        let parsed = Ulid::from_string(&string).unwrap();
        assert_eq!(original, parsed);
    }
}

// ============================================================================
// Activity Route Tests
// ============================================================================

#[cfg(test)]
mod activity_tests {
    use crate::routes::activity::{ActivityEventResponse, ActivityListResponse};
    use chrono::Utc;
    use hof_core::domain::activity::{ActivityEventType, ActivitySeverity};

    #[test]
    fn test_activity_event_response_schema() {
        let response = ActivityEventResponse {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            event_type: ActivityEventType::DownloadStarted,
            severity: ActivitySeverity::Info,
            message: "Started downloading video".to_string(),
            source_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string()),
            video_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAX".to_string()),
            profile_id: None,
            created_at: Utc::now(),
            source_indexing: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("DownloadStarted"));
        assert!(json.contains("Info"));
        assert!(json.contains("Started downloading video"));
    }

    #[test]
    fn test_activity_list_response_schema() {
        let response = ActivityListResponse {
            events: vec![],
            total: 0,
            limit: 50,
            offset: 0,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"total\":0"));
        assert!(json.contains("\"limit\":50"));
        assert!(json.contains("\"offset\":0"));
    }

    #[test]
    fn test_activity_event_type_serialization() {
        assert_eq!(
            serde_json::to_string(&ActivityEventType::DownloadStarted).unwrap(),
            "\"DownloadStarted\""
        );
        assert_eq!(
            serde_json::to_string(&ActivityEventType::DownloadCompleted).unwrap(),
            "\"DownloadCompleted\""
        );
        assert_eq!(
            serde_json::to_string(&ActivityEventType::SourceIndexed).unwrap(),
            "\"SourceIndexed\""
        );
    }

    #[test]
    fn test_activity_severity_serialization() {
        // Note: ActivitySeverity has variants: Info, Success, Warning, Error
        assert_eq!(
            serde_json::to_string(&ActivitySeverity::Info).unwrap(),
            "\"Info\""
        );
        assert_eq!(
            serde_json::to_string(&ActivitySeverity::Success).unwrap(),
            "\"Success\""
        );
        assert_eq!(
            serde_json::to_string(&ActivitySeverity::Warning).unwrap(),
            "\"Warning\""
        );
        assert_eq!(
            serde_json::to_string(&ActivitySeverity::Error).unwrap(),
            "\"Error\""
        );
    }
}
