//! Shared library for the herdr token-usage collector and its price importer.

pub mod cost;
pub mod db;
pub mod event;
pub mod sources;
pub mod tail;

use std::path::PathBuf;

/// Default database location: outside the repository, alongside the collector.
pub fn default_db_path() -> PathBuf {
    home_dir().join(".local/share/herdr/usage.db")
}

/// Resolve the database path, honouring `HERDR_USAGE_DB`.
pub fn db_path() -> PathBuf {
    std::env::var_os("HERDR_USAGE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(default_db_path)
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
