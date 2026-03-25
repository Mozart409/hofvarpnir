//! System-level domain types for health and startup issues.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Severity level for system issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    /// Warning: system is degraded but functional.
    Warning,
    /// Error: critical functionality is broken.
    Error,
}

/// A system issue detected during startup or runtime.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SystemIssue {
    /// Unique identifier for the issue type.
    pub code: String,
    /// Severity of the issue.
    pub severity: IssueSeverity,
    /// Human-readable message describing the issue.
    pub message: String,
}

impl SystemIssue {
    /// Create a new error-level system issue.
    #[must_use]
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: IssueSeverity::Error,
            message: message.into(),
        }
    }

    /// Create a new warning-level system issue.
    #[must_use]
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: IssueSeverity::Warning,
            message: message.into(),
        }
    }
}
