//! Integration tests for API routes.
//!
//! These tests focus on request/response validation, serialization,
//! ULID parsing, and error handling for API types.

// ============================================================================
// Profile Route Tests
// ============================================================================

#[cfg(test)]
mod profile_tests {
    use crate::routes::profiles::UpdateProfileRequest;

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
}

// ============================================================================
// Source Route Tests
// ============================================================================

#[cfg(test)]
mod source_tests {
    use crate::routes::sources::UpdateSourceRequest;

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
}

// ============================================================================
// Download Route Tests
// ============================================================================

#[cfg(test)]
mod download_tests {
    use crate::routes::downloads::VideoResponse;

    #[test]
    fn test_video_response_extracts_machine_error_code() {
        use chrono::Utc;
        use hof_core::domain::video::Video;
        use hof_core::domain::video::VideoStatus;
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
