//! # Overview
//!
//! A session storage that stores each session in a seperate file in a special folder (default ".sessions")
//!
//! Useful when you want something more persistant than a in memory store but don't want to setup an entire database, especially
//! during local development. Should work fine in production environments though.
//!
//! # Expiry
//!
//! You can enable automatically deleting expired sessions like this:
//!
//! ```rs
//! let deletion_task = tokio::task::spawn(
//!     session_store
//!     .clone()
//!     .continuously_delete_expired(tokio::time::Duration::from_secs(60)),
//! );
//! ```
//!
//! By default, it will only load sessions to check their expirty if the last modified date of the file is at least 60 seconds. You can adjust this with
//! `set_minimum_expiry_date`. Ideally the expiry date would be the same as the duration of your sessions.

use std::{
    borrow::Cow,
    fs::OpenOptions,
    path::Path,
    str::FromStr,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::fs::remove_file;
use tower_sessions_core::{
    session::{Id, Record},
    session_store, ExpiredDeletion, SessionStore,
};

/// A Session storage that stores each session, JSON encoded, on the local disk.
///
/// In production, you may want to put this behind a [`MemoryStore`](https://docs.rs/tower-sessions/latest/tower_sessions/struct.MemoryStore.html)
/// for performance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSessionStorage {
    folder_name: Cow<'static, Path>,
    minimum_expiry_date: Duration,
}

impl Default for FileSessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSessionStorage {
    /// Create a new `FileSessionStore`` in the folder ".sessions"
    pub fn new() -> FileSessionStorage {
        FileSessionStorage::new_in_folder(Path::new(".sessions"))
    }

    /// Create a new `FileSessionStore` with sessions placed in the given folder.
    pub fn new_in_folder(folder: impl Into<Cow<'static, Path>>) -> Self {
        FileSessionStorage {
            folder_name: folder.into(),
            minimum_expiry_date: Duration::from_secs(60),
        }
    }

    /// We need to open every session file to determine if it expired.
    /// The minimum expiry time sets the minimum age of a file before attempting to open it.
    pub fn set_minimum_expiry_date(mut self, duration: Duration) -> Self {
        self.minimum_expiry_date = duration;
        self
    }
}

#[async_trait]
impl SessionStore for FileSessionStorage {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        tokio::fs::create_dir_all(&self.folder_name)
            .await
            .map_err(|_| session_store::Error::Backend("Failed to create folder".to_string()))?;

        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.folder_name.join(record.id.to_string()))
            .map_err(|_| session_store::Error::Backend("Failed to open file".to_string()))?;
        serde_json::to_writer(file, &record)
            .map_err(|_| session_store::Error::Backend("Failed to serialize/decode".to_string()))?;

        Ok(())
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(self.folder_name.join(record.id.to_string()))
            .map_err(|_| session_store::Error::Backend("Failed to open file".to_string()))?;
        serde_json::to_writer(file, &record)
            .map_err(|_| session_store::Error::Backend("Failed to serialize/decode".to_string()))?;
        Ok(())
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let path = self.folder_name.join(session_id.to_string());
        if !path.is_file() {
            return Ok(None);
        }
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|_| session_store::Error::Backend("Failed to open file".to_string()))?;
        let out = serde_json::from_reader(file)
            .map_err(|_| session_store::Error::Backend("Failed to serialize/decode".to_string()))?;

        Ok(out)
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let res = remove_file(self.folder_name.join(session_id.to_string())).await;
        match res {
            Ok(_) => {}
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(session_store::Error::Backend(
                        "Failed to Delete".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ExpiredDeletion for FileSessionStorage {
    async fn delete_expired(&self) -> session_store::Result<()> {
        let mut folders = tokio::fs::read_dir(&self.folder_name)
            .await
            .map_err(|_| session_store::Error::Backend("Failed to list folder".to_string()))?;
        while let Some(dir_entry) = folders
            .next_entry()
            .await
            .map_err(|_| session_store::Error::Backend("Failed to load next file".to_string()))?
        {
            let Some(session_id) = dir_entry
                .file_name()
                .to_str()
                .and_then(|k| Id::from_str(k).ok())
            else {
                continue;
            };
            let metadata = dir_entry
                .metadata()
                .await
                .map_err(|_| session_store::Error::Backend("Failed to get metadata".to_string()))?;
            let modified_date = metadata.modified().map_err(|_| {
                session_store::Error::Backend("Failed to get modified date".to_string())
            })?;
            let age = SystemTime::now()
                .duration_since(modified_date)
                .map_err(|_| {
                    session_store::Error::Backend("Failed to subtract dates".to_string())
                })?;
            if age < self.minimum_expiry_date {
                continue;
            }

            let Some(session) = self.load(&session_id).await? else {
                continue;
            };
            if OffsetDateTime::now_utc() > session.expiry_date {
                self.delete(&session_id).await?;
            }
        }

        Ok(())
    }
}
