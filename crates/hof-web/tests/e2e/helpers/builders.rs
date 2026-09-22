//! Test data builders for seeding the database.

use chrono::NaiveDate;
use hof_core::{
    db::{self, CreateProfile, CreateSource, CreateUser},
    domain::{
        activity::{ActivityEventType, ActivitySeverity},
        profile::{OutputPreset, Profile, Quality},
        source::{Source, SourceType},
        user::User,
        video::{Video, VideoStatus},
    },
};
use sqlx::{PgPool, Row};
use ulid::Ulid;

/// The password every `UserBuilder` user gets unless told otherwise.
///
/// `TestWebApp::login_as` posts this to `/login`, so the hash written here has
/// to be a real argon2 PHC string produced by `hof_core::auth::hash_password` —
/// a hand-written placeholder never verifies and the login silently falls
/// through to "Invalid email or password".
pub const TEST_PASSWORD: &str = "correct-horse-battery-staple";

/// Builder for creating test users.
pub struct UserBuilder {
    name: String,
    email: String,
    password: String,
}

impl UserBuilder {
    /// Create a new user builder with random defaults.
    #[must_use]
    pub fn new() -> Self {
        let id = Ulid::generate();
        Self {
            name: format!("Test User {id}"),
            email: format!("test_{id}@example.com"),
            password: TEST_PASSWORD.to_string(),
        }
    }

    /// Set the name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the email.
    #[must_use]
    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = email.into();
        self
    }

    /// Set the password (hashed with argon2 on build).
    #[must_use]
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = password.into();
        self
    }

    /// Build and insert the user into the database.
    pub async fn build(self, pool: &PgPool) -> User {
        let hash =
            hof_core::auth::hash_password(&self.password).expect("Failed to hash test password");

        db::create_user(
            pool,
            CreateUser {
                name: &self.name,
                email: &self.email,
                password_hash: Some(&hash),
            },
        )
        .await
        .expect("Failed to create test user")
    }
}

impl Default for UserBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for creating test profiles.
pub struct ProfileBuilder {
    user_id: Ulid,
    name: String,
    quality: Quality,
    output_preset: OutputPreset,
}

impl ProfileBuilder {
    /// Create a new profile builder for a user.
    #[must_use]
    pub fn new(user_id: Ulid) -> Self {
        let id = Ulid::generate();
        Self {
            user_id,
            name: format!("Test Profile {id}"),
            quality: Quality::Q1080p,
            output_preset: OutputPreset::Browser,
        }
    }

    /// Set the profile name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the quality.
    #[must_use]
    pub fn quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }

    /// Build and insert the profile into the database.
    pub async fn build(self, pool: &PgPool) -> Profile {
        db::create_profile(
            pool,
            CreateProfile {
                user_id: self.user_id,
                name: &self.name,
                quality: self.quality,
                output_preset: self.output_preset,
                naming_template: "{title}-{id}.{ext}",
                output_dir: "/tmp/test_downloads",
                include_livestreams: false,
                include_shorts: false,
                storage_quota_bytes: 100_000_000_000,
                retention_days: None,
            },
        )
        .await
        .expect("Failed to create test profile")
    }
}

/// Builder for creating test sources.
pub struct SourceBuilder {
    profile_id: Ulid,
    url: String,
    custom_name: Option<String>,
    exclude_from_cleanup: bool,
}

impl SourceBuilder {
    /// Create a new source builder for a profile.
    #[must_use]
    pub fn new(profile_id: Ulid) -> Self {
        let id = Ulid::generate();
        Self {
            profile_id,
            url: format!("https://youtube.com/@test_channel_{id}"),
            custom_name: None,
            exclude_from_cleanup: false,
        }
    }

    /// Set the source URL.
    #[must_use]
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Set a custom name.
    #[must_use]
    pub fn custom_name(mut self, name: impl Into<String>) -> Self {
        self.custom_name = Some(name.into());
        self
    }

    /// Exempt this source's videos from automatic cleanup.
    #[must_use]
    pub fn exclude_from_cleanup(mut self) -> Self {
        self.exclude_from_cleanup = true;
        self
    }

    /// Build and insert the source into the database.
    pub async fn build(self, pool: &PgPool) -> Source {
        let source = db::create_source(
            pool,
            CreateSource {
                profile_id: self.profile_id,
                url: &self.url,
                source_type: SourceType::Channel,
                custom_name: self.custom_name.as_deref(),
                index_frequency_secs: 3600,
                cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date"),
                retention_days: None,
            },
        )
        .await
        .expect("Failed to create test source");

        if self.exclude_from_cleanup {
            db::set_source_exclude_from_cleanup(pool, source.id, true)
                .await
                .expect("Failed to exclude test source from cleanup");
        }

        if self.exclude_from_cleanup {
            db::get_source(pool, source.id)
                .await
                .expect("Failed to reload test source")
        } else {
            source
        }
    }
}

/// Builder for creating test videos.
pub struct VideoBuilder {
    source_id: Ulid,
    title: String,
    platform_video_id: String,
    status: VideoStatus,
    duration_secs: Option<i64>,
    video_height: Option<i32>,
    video_codec: Option<String>,
    last_error: Option<String>,
    published_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl VideoBuilder {
    /// Create a new video builder for a source.
    #[must_use]
    pub fn new(source_id: Ulid) -> Self {
        let id = Ulid::generate();
        Self {
            source_id,
            title: format!("Test Video {id}"),
            platform_video_id: format!("video_{id}"),
            status: VideoStatus::Pending,
            duration_secs: Some(3600),
            video_height: None,
            video_codec: None,
            last_error: None,
            published_at: None,
        }
    }

    /// Set the publish date, which is what the UI orders listings by.
    #[must_use]
    pub fn published_at(mut self, published_at: chrono::DateTime<chrono::Utc>) -> Self {
        self.published_at = Some(published_at);
        self
    }

    /// Set the video title.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the video status.
    #[must_use]
    pub fn status(mut self, status: VideoStatus) -> Self {
        self.status = status;
        self
    }

    /// Set the delivered video height.
    #[must_use]
    pub fn video_height(mut self, height: i32) -> Self {
        self.video_height = Some(height);
        self
    }

    /// Set the delivered video codec.
    #[must_use]
    pub fn video_codec(mut self, codec: impl Into<String>) -> Self {
        self.video_codec = Some(codec.into());
        self
    }

    /// Set the last error message.
    #[must_use]
    pub fn last_error(mut self, error: impl Into<String>) -> Self {
        self.last_error = Some(error.into());
        self
    }

    /// Build and insert the video into the database.
    pub async fn build(self, pool: &PgPool) -> Video {
        let video_id = Ulid::generate();
        let status_str = match self.status {
            VideoStatus::Pending => "pending",
            VideoStatus::Downloading => "downloading",
            VideoStatus::Completed => "completed",
            VideoStatus::Failed => "failed",
            VideoStatus::Skipped => "skipped",
            VideoStatus::Cleaned => "cleaned",
            VideoStatus::PermanentlyFailed => "permanently_failed",
        };

        let sql = sqlx::query(
            "INSERT INTO videos (
                id, platform, platform_video_id, title, status, duration_secs,
                video_height, video_codec, last_error, published_at
            )
            VALUES ($1, $2, $3, $4, $5::video_status, $6, $7, $8, $9, $10)
            RETURNING id, platform, platform_video_id, title, status::text, duration_secs,
                      video_height, video_codec, last_error, created_at, updated_at",
        )
        .bind(video_id.to_string())
        .bind("youtube")
        .bind(&self.platform_video_id)
        .bind(&self.title)
        .bind(status_str)
        .bind(self.duration_secs)
        .bind(self.video_height)
        .bind(self.video_codec.clone())
        .bind(&self.last_error)
        .bind(self.published_at);

        let video = sql
            .fetch_one(pool)
            .await
            .expect("Failed to create test video");

        let id_str: String = video.get("id");
        let status_str_returned: String = video.get("status");
        let created_at: chrono::DateTime<chrono::Utc> = video.get("created_at");
        let updated_at: chrono::DateTime<chrono::Utc> = video.get("updated_at");

        // Link video to source
        sqlx::query("INSERT INTO source_videos (source_id, video_id) VALUES ($1, $2)")
            .bind(self.source_id.to_string())
            .bind(&id_str)
            .execute(pool)
            .await
            .expect("Failed to link video to source");

        Video {
            id: Ulid::from_string(&id_str).expect("valid ulid"),
            platform: "youtube".to_string(),
            platform_video_id: self.platform_video_id,
            title: self.title,
            description: None,
            duration_secs: self.duration_secs,
            published_at: self.published_at,
            thumbnail_url: None,
            status: match status_str_returned.as_str() {
                "downloading" => VideoStatus::Downloading,
                "completed" => VideoStatus::Completed,
                "failed" => VideoStatus::Failed,
                "skipped" => VideoStatus::Skipped,
                "cleaned" => VideoStatus::Cleaned,
                "permanently_failed" => VideoStatus::PermanentlyFailed,
                _ => VideoStatus::Pending,
            },
            attempts: 0,
            next_retry: None,
            last_error: self.last_error,
            file_path: None,
            file_size_bytes: None,
            downloaded_at: None,
            video_height: self.video_height,
            video_codec: self.video_codec,
            created_at,
            updated_at,
        }
    }
}

/// Builder for creating test activity events.
pub struct ActivityBuilder {
    event_type: ActivityEventType,
    severity: ActivitySeverity,
    message: String,
    source_id: Option<Ulid>,
}

impl ActivityBuilder {
    /// Create a new activity builder.
    #[must_use]
    pub fn new(
        event_type: ActivityEventType,
        severity: ActivitySeverity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            event_type,
            severity,
            message: message.into(),
            source_id: None,
        }
    }

    /// Set the source ID.
    #[must_use]
    pub fn source_id(mut self, source_id: Ulid) -> Self {
        self.source_id = Some(source_id);
        self
    }

    /// Build and insert the activity event into the database.
    pub async fn build(self, pool: &PgPool) {
        let id = Ulid::generate();

        let event_type_str = match self.event_type {
            ActivityEventType::SourceIndexed => "source_indexed",
            ActivityEventType::SourceError => "source_error",
            ActivityEventType::DownloadStarted => "download_started",
            ActivityEventType::DownloadCompleted => "download_completed",
            ActivityEventType::DownloadFailed => "download_failed",
            ActivityEventType::RetryScheduled => "retry_scheduled",
            ActivityEventType::MetadataGenerated => "metadata_generated",
            ActivityEventType::VideoCleaned => "video_cleaned",
            ActivityEventType::ProfileCreated => "profile_created",
            ActivityEventType::ProfileUpdated => "profile_updated",
            ActivityEventType::ProfileDeleted => "profile_deleted",
            ActivityEventType::SourceCreated => "source_created",
            ActivityEventType::SourceUpdated => "source_updated",
            ActivityEventType::SourceDeleted => "source_deleted",
            ActivityEventType::SelfRestart => "self_restart",
        };

        let severity_str = match self.severity {
            ActivitySeverity::Info => "info",
            ActivitySeverity::Success => "success",
            ActivitySeverity::Warning => "warning",
            ActivitySeverity::Error => "error",
        };

        sqlx::query(
            r"
            INSERT INTO activity_events (id, event_type, severity, message, source_id)
            VALUES ($1, $2::activity_event_type, $3::activity_severity, $4, $5)
            ",
        )
        .bind(id.to_string())
        .bind(event_type_str)
        .bind(severity_str)
        .bind(&self.message)
        .bind(self.source_id.map(|id| id.to_string()))
        .execute(pool)
        .await
        .expect("Failed to create test activity event");
    }
}
