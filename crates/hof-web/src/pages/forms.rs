//! Split out of `pages/mod.rs`.

use super::*;

#[derive(Debug, Deserialize)]
pub(super) enum QualityForm {
    #[serde(rename = "best")]
    Best,
    #[serde(rename = "4320p")]
    Q4320p,
    #[serde(rename = "2160p")]
    Q2160p,
    #[serde(rename = "1440p")]
    Q1440p,
    #[serde(rename = "1080p")]
    Q1080p,
    #[serde(rename = "720p")]
    Q720p,
    #[serde(rename = "480p")]
    Q480p,
    #[serde(rename = "audio_only")]
    AudioOnly,
}

impl From<QualityForm> for Quality {
    fn from(value: QualityForm) -> Self {
        match value {
            QualityForm::Best => Self::Best,
            QualityForm::Q4320p => Self::Q4320p,
            QualityForm::Q2160p => Self::Q2160p,
            QualityForm::Q1440p => Self::Q1440p,
            QualityForm::Q1080p => Self::Q1080p,
            QualityForm::Q720p => Self::Q720p,
            QualityForm::Q480p => Self::Q480p,
            QualityForm::AudioOnly => Self::AudioOnly,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum OutputPresetForm {
    Auto,
    Browser,
    Tv,
}

impl From<OutputPresetForm> for OutputPreset {
    fn from(value: OutputPresetForm) -> Self {
        match value {
            OutputPresetForm::Auto => Self::Auto,
            OutputPresetForm::Browser => Self::Browser,
            OutputPresetForm::Tv => Self::Tv,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SourceTypeForm {
    Channel,
    Playlist,
}

impl From<SourceTypeForm> for SourceType {
    fn from(value: SourceTypeForm) -> Self {
        match value {
            SourceTypeForm::Channel => Self::Channel,
            SourceTypeForm::Playlist => Self::Playlist,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ProfileForm {
    pub(super) name: String,
    pub(super) quality: QualityForm,
    pub(super) output_preset: OutputPresetForm,
    pub(super) naming_template: String,
    pub(super) output_dir: String,
    pub(super) include_livestreams: Option<String>,
    pub(super) include_shorts: Option<String>,
    pub(super) storage_quota_gb: i64,
    pub(super) retention_days: Option<String>,
    pub(super) quality_wait_hours: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourceForm {
    pub(super) profile_id: String,
    pub(super) url: String,
    pub(super) source_type: SourceTypeForm,
    pub(super) custom_name: Option<String>,
    pub(super) index_frequency_secs: i64,
    pub(super) cutoff_date: String,
    pub(super) retention_days: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyForm {
    pub(super) name: String,
    pub(super) scope_read: Option<String>,
    pub(super) scope_write: Option<String>,
    pub(super) scope_delete: Option<String>,
    pub(super) expires_in: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub(super) email: String,
    pub(super) password: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterForm {
    pub(super) name: String,
    pub(super) email: String,
    pub(super) password: String,
    pub(super) password_confirm: String,
}
