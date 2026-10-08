//! `/` search: query terms and row matching.
//!
//! Every tab but Activity filters its loaded rows on the client: the query is
//! split into whitespace-separated terms, and a row is shown when each term
//! is a case-insensitive substring of one of its text fields or a prefix of
//! one of its keywords. Keywords are for columns shown as symbols (a source's
//! `●`/`○` is `enabled`/`disabled`); prefix matching keeps `dis` from also
//! matching `enabled`, which a substring match on `disabled` would not.
//!
//! Activity is paged from the server, so its query becomes an
//! [`ActivityFilter`] sent with the request instead.

use crate::types::ActivitySeverity;

/// Lower-cased whitespace-separated terms of a query; empty matches all.
pub fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether every term matches one of `fields` (substring) or `keywords`
/// (prefix). Both are compared case-insensitively; keywords are expected
/// to be lower-case already.
pub fn matches(terms: &[String], fields: &[&str], keywords: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    terms.iter().all(|term| {
        fields.iter().any(|f| f.contains(term.as_str()))
            || keywords.iter().any(|k| k.starts_with(term.as_str()))
    })
}

/// Server-side filter for the Activity tab, parsed from its query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityFilter {
    /// From a term naming a severity (`error`, `warn`, `info`, `ok`, ...).
    pub severity: Option<ActivitySeverity>,
    /// The remaining terms, matched by the server as one substring of the
    /// message.
    pub search: Option<String>,
}

impl ActivityFilter {
    /// The first term naming a severity becomes the severity filter; every
    /// other term is part of the message search.
    pub fn parse(query: &str) -> Self {
        let mut severity = None;
        let mut rest = Vec::new();
        for term in query.split_whitespace() {
            match severity_word(term) {
                Some(s) if severity.is_none() => severity = Some(s),
                _ => rest.push(term),
            }
        }
        Self {
            severity,
            search: (!rest.is_empty()).then(|| rest.join(" ")),
        }
    }

    /// The filter as the query it was parsed from, normalized.
    pub fn describe(&self) -> String {
        let severity = self.severity.map(ActivitySeverity::label);
        severity
            .into_iter()
            .chain(self.search.as_deref())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub const fn is_empty(&self) -> bool {
        self.severity.is_none() && self.search.is_none()
    }
}

/// A severity named by its table label or full name, in any case.
fn severity_word(term: &str) -> Option<ActivitySeverity> {
    match term.to_lowercase().as_str() {
        "info" => Some(ActivitySeverity::Info),
        "ok" | "success" => Some(ActivitySeverity::Success),
        "warn" | "warning" => Some(ActivitySeverity::Warning),
        "err" | "error" => Some(ActivitySeverity::Error),
        _ => None,
    }
}
