//! Profile test builder.

use hof_core::{
    db::{self, CreateProfile},
    domain::profile::{Profile, Quality},
};
use sqlx::PgPool;
use ulid::Ulid;

/// Builder for creating test profiles.
pub struct ProfileBuilder {
    user_id: Ulid,
    name: String,
    quality: Quality,
    naming_template: String,
    output_dir: String,
    include_livestreams: bool,
    include_shorts: bool,
    storage_quota_bytes: i64,
    retention_days: Option<i32>,
}

impl ProfileBuilder {
    /// Create a new profile builder for a user.
    #[must_use]
    pub fn new(user_id: Ulid) -> Self {
        let id = Ulid::new();
        Self {
            user_id,
            name: format!("Test Profile {id}"),
            quality: Quality::Q1080p,
            naming_template: "{title}-{id}.{ext}".to_string(),
            output_dir: format!("/tmp/test_downloads_{id}"),
            include_livestreams: false,
            include_shorts: false,
            storage_quota_bytes: 100 * 1024 * 1024 * 1024, // 100 GB
            retention_days: None,
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

    /// Set the naming template.
    #[must_use]
    pub fn naming_template(mut self, template: impl Into<String>) -> Self {
        self.naming_template = template.into();
        self
    }

    /// Set the output directory.
    #[must_use]
    pub fn output_dir(mut self, dir: impl Into<String>) -> Self {
        self.output_dir = dir.into();
        self
    }

    /// Enable livestreams.
    #[must_use]
    pub fn with_livestreams(mut self) -> Self {
        self.include_livestreams = true;
        self
    }

    /// Enable shorts.
    #[must_use]
    pub fn with_shorts(mut self) -> Self {
        self.include_shorts = true;
        self
    }

    /// Set storage quota.
    #[must_use]
    pub fn storage_quota_bytes(mut self, bytes: i64) -> Self {
        self.storage_quota_bytes = bytes;
        self
    }

    /// Set retention days.
    #[must_use]
    pub fn retention_days(mut self, days: i32) -> Self {
        self.retention_days = Some(days);
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
                naming_template: &self.naming_template,
                output_dir: &self.output_dir,
                include_livestreams: self.include_livestreams,
                include_shorts: self.include_shorts,
                storage_quota_bytes: self.storage_quota_bytes,
                retention_days: self.retention_days,
            },
        )
        .await
        .expect("Failed to create test profile")
    }
}
