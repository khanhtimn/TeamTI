use async_trait::async_trait;
use uuid::Uuid;

use crate::db_err;
use application::PLAYLIST_COLLABORATOR_LIMIT;
use application::error::{AppError, PlaylistErrorKind};
use application::ports::playlist::PlaylistPort;
use domain::track::TrackSummary;
use domain::user_library::{
    Playlist, PlaylistItem, PlaylistPage, PlaylistSummary, PlaylistVisibility,
};

use crate::db::Database;

pub struct PgPlaylistRepository {
    db: Database,
}

impl PgPlaylistRepository {
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Check if a user has write access (owner or collaborator).
    async fn check_write_access(&self, playlist_id: Uuid, user_id: &str) -> Result<(), AppError> {
        let row = sqlx::query_scalar!("SELECT owner_id FROM playlists WHERE id = $1", playlist_id)
            .fetch_optional(&self.db.0)
            .await
            .map_err(db_err!("playlist.check_write_access"))?;

        let Some(owner_id) = row else {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("playlist {playlist_id} not found"),
            });
        };

        if owner_id == user_id {
            return Ok(());
        }

        // Check collaborator
        let is_collab: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM playlist_collaborators WHERE playlist_id = $1 AND user_id = $2) AS "is_collab!""#,
            playlist_id,
            user_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.check_collaborator"))?;

        if is_collab {
            Ok(())
        } else {
            Err(AppError::Playlist {
                kind: PlaylistErrorKind::Forbidden,
                detail: format!("user {user_id} has no write access to playlist {playlist_id}"),
            })
        }
    }

    /// Check if a user can read a playlist (owner, collaborator, or public).
    async fn check_read_access(
        &self,
        playlist_id: Uuid,
        user_id: &str,
    ) -> Result<String, AppError> {
        // Returns owner_id if accessible
        #[derive(sqlx::FromRow)]
        struct PlaylistAccess {
            owner_id: String,
            visibility: String,
        }

        let row = sqlx::query_as!(
            PlaylistAccess,
            "SELECT owner_id, visibility FROM playlists WHERE id = $1",
            playlist_id
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(db_err!("playlist.check_read_access"))?;

        let Some(access) = row else {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("playlist {playlist_id} not found"),
            });
        };

        if access.owner_id == user_id || access.visibility == "public" {
            return Ok(access.owner_id);
        }

        // Check collaborator
        let is_collab: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM playlist_collaborators WHERE playlist_id = $1 AND user_id = $2) AS "is_collab!""#,
            playlist_id,
            user_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.check_read_collaborator"))?;

        if is_collab {
            Ok(access.owner_id)
        } else {
            // Return NotFound to avoid leaking existence of private playlists
            Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("playlist {playlist_id} not found"),
            })
        }
    }
}

#[async_trait]
impl PlaylistPort for PgPlaylistRepository {
    async fn create_playlist(
        &self,
        owner_id: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<Playlist, AppError> {
        #[derive(sqlx::FromRow)]
        struct PlaylistRow {
            id: Uuid,
            name: String,
            owner_id: String,
            visibility: String,
            description: Option<String>,
            created_at: chrono::DateTime<chrono::Utc>,
            updated_at: chrono::DateTime<chrono::Utc>,
        }
        // Check for duplicate name for this owner
        let exists: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM playlists WHERE owner_id = $1 AND name = $2) AS "exists!""#,
            owner_id,
            name
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.create.check_duplicate"))?;

        if exists {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::AlreadyExists,
                detail: format!("playlist '{name}' already exists for user {owner_id}"),
            });
        }

        let row = sqlx::query_as!(
            PlaylistRow,
            "INSERT INTO playlists (name, owner_id, description) VALUES ($1, $2, $3)
             RETURNING id, name, owner_id, visibility, description, created_at, updated_at",
            name,
            owner_id,
            description
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.create"))?;

        Ok(Playlist {
            id: row.id,
            name: row.name,
            owner_id: row.owner_id,
            visibility: row
                .visibility
                .parse()
                .unwrap_or(PlaylistVisibility::Private),
            description: row.description,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    async fn rename_playlist(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        new_name: &str,
    ) -> Result<(), AppError> {
        let rows = sqlx::query!(
            "UPDATE playlists SET name = $1, updated_at = now() WHERE id = $2 AND owner_id = $3",
            new_name,
            playlist_id,
            owner_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.rename"))?;

        if rows.rows_affected() == 0 {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("playlist {playlist_id} not found or not owned by {owner_id}"),
            });
        }
        Ok(())
    }

    async fn delete_playlist(&self, playlist_id: Uuid, owner_id: &str) -> Result<(), AppError> {
        let rows = sqlx::query!(
            "DELETE FROM playlists WHERE id = $1 AND owner_id = $2",
            playlist_id,
            owner_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.delete"))?;

        if rows.rows_affected() == 0 {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("playlist {playlist_id} not found or not owned by {owner_id}"),
            });
        }
        Ok(())
    }

    async fn set_visibility(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        visibility: PlaylistVisibility,
    ) -> Result<(), AppError> {
        let rows = sqlx::query!(
            "UPDATE playlists SET visibility = $1, updated_at = now() WHERE id = $2 AND owner_id = $3",
            visibility.as_str(), playlist_id, owner_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.set_visibility"))?;

        if rows.rows_affected() == 0 {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("playlist {playlist_id} not found or not owned by {owner_id}"),
            });
        }
        Ok(())
    }

    async fn add_track(
        &self,
        playlist_id: Uuid,
        track_id: Uuid,
        added_by: &str,
    ) -> Result<PlaylistItem, AppError> {
        self.check_write_access(playlist_id, added_by).await?;

        // Get the next position (max + 1)
        let max_pos: Option<i32> = sqlx::query_scalar!(
            "SELECT MAX(position) FROM playlist_items WHERE playlist_id = $1",
            playlist_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.add_track.max_pos"))?;

        let next_pos = max_pos.map_or(0, |p| p + 1);

        let item = sqlx::query_as::<_, PlaylistItemRow>(
            "INSERT INTO playlist_items (playlist_id, track_id, position, added_by)
             VALUES ($1, $2, $3, $4)
             RETURNING id, playlist_id, track_id, position, added_by, added_at",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(next_pos)
        .bind(added_by)
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.add_track"))?;

        // Touch updated_at
        let _ = sqlx::query!(
            "UPDATE playlists SET updated_at = now() WHERE id = $1",
            playlist_id
        )
        .execute(&self.db.0)
        .await;

        Ok(item.into())
    }

    async fn remove_track(
        &self,
        playlist_id: Uuid,
        item_id: Uuid,
        requesting_user: &str,
    ) -> Result<(), AppError> {
        self.check_write_access(playlist_id, requesting_user)
            .await?;

        let rows = sqlx::query!(
            "DELETE FROM playlist_items WHERE id = $1 AND playlist_id = $2",
            item_id,
            playlist_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.remove_track"))?;

        if rows.rows_affected() == 0 {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("item {item_id} not found in playlist {playlist_id}"),
            });
        }

        // Touch updated_at
        let _ = sqlx::query!(
            "UPDATE playlists SET updated_at = now() WHERE id = $1",
            playlist_id
        )
        .execute(&self.db.0)
        .await;

        Ok(())
    }

    async fn reorder_track(
        &self,
        playlist_id: Uuid,
        item_id: Uuid,
        new_position: i32,
        requesting_user: &str,
    ) -> Result<(), AppError> {
        self.check_write_access(playlist_id, requesting_user)
            .await?;

        let rows = sqlx::query!(
            "UPDATE playlist_items SET position = $1 WHERE id = $2 AND playlist_id = $3",
            new_position,
            item_id,
            playlist_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.reorder_track"))?;

        if rows.rows_affected() == 0 {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::NotFound,
                detail: format!("item {item_id} not found in playlist {playlist_id}"),
            });
        }

        // Touch updated_at
        let _ = sqlx::query!(
            "UPDATE playlists SET updated_at = now() WHERE id = $1",
            playlist_id
        )
        .execute(&self.db.0)
        .await;

        Ok(())
    }

    async fn list_user_playlists(&self, owner_id: &str) -> Result<Vec<PlaylistSummary>, AppError> {
        let rows = sqlx::query_as!(
            PlaylistSummaryRow,
            r#"SELECT p.id, p.name, p.owner_id, p.visibility,
                    COALESCE(COUNT(pi.id), 0) AS "track_count!"
             FROM playlists p
             LEFT JOIN playlist_items pi ON pi.playlist_id = p.id
             WHERE p.owner_id = $1
             GROUP BY p.id
             ORDER BY p.updated_at DESC"#,
            owner_id
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("playlist.list_user"))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn list_accessible_playlists(
        &self,
        user_id: &str,
    ) -> Result<Vec<PlaylistSummary>, AppError> {
        let rows = sqlx::query_as!(
            PlaylistSummaryRow,
            r#"SELECT p.id, p.name, p.owner_id, p.visibility,
                    COALESCE(COUNT(pi.id), 0) AS "track_count!"
             FROM playlists p
             LEFT JOIN playlist_items pi ON pi.playlist_id = p.id
             WHERE p.owner_id = $1
                OR p.visibility = 'public'
                OR EXISTS(SELECT 1 FROM playlist_collaborators pc
                          WHERE pc.playlist_id = p.id AND pc.user_id = $1)
             GROUP BY p.id
             ORDER BY p.updated_at DESC"#,
            user_id
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("playlist.list_accessible"))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn get_playlist_items(
        &self,
        playlist_id: Uuid,
        requesting_user: &str,
        page: i64,
        page_size: i64,
    ) -> Result<PlaylistPage, AppError> {
        self.check_read_access(playlist_id, requesting_user).await?;

        let total: i64 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM playlist_items WHERE playlist_id = $1"#,
            playlist_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.get_items.count"))?;

        let offset = page * page_size;

        let rows = sqlx::query_as!(
            PlaylistItemWithTrack,
            r#"SELECT pi.id AS item_id, pi.playlist_id, pi.track_id, pi.position,
                    pi.added_by, pi.added_at,
                    t.title, t.artist_display, a.title AS "album_title?",
                    t.album_id, t.duration_ms, t.blob_location
             FROM playlist_items pi
             JOIN tracks t ON t.id = pi.track_id
             LEFT JOIN albums a ON a.id = t.album_id
             WHERE pi.playlist_id = $1
             ORDER BY pi.position ASC, pi.added_at ASC
             LIMIT $2 OFFSET $3"#,
            playlist_id,
            page_size,
            offset
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("playlist.get_items"))?;

        let items = rows
            .into_iter()
            .map(|r| {
                let item = PlaylistItem {
                    id: r.item_id,
                    playlist_id: r.playlist_id,
                    track_id: r.track_id,
                    position: r.position,
                    added_by: r.added_by,
                    added_at: r.added_at,
                };
                let summary = TrackSummary {
                    id: r.track_id,
                    title: r.title,
                    artist_display: r.artist_display,
                    album_title: r.album_title,
                    album_id: r.album_id,
                    duration_ms: r.duration_ms,
                    blob_location: r.blob_location,
                };
                (item, summary)
            })
            .collect();

        Ok(PlaylistPage {
            items,
            total,
            page,
            page_size,
        })
    }

    async fn get_playlist_tracks(
        &self,
        playlist_id: Uuid,
        requesting_user: &str,
    ) -> Result<Vec<TrackSummary>, AppError> {
        self.check_read_access(playlist_id, requesting_user).await?;

        let rows = sqlx::query_as!(
            TrackSummaryRow,
            r#"SELECT t.id, t.title, t.artist_display, a.title AS "album_title?",
                    t.album_id, t.duration_ms, t.blob_location
             FROM playlist_items pi
             JOIN tracks t ON t.id = pi.track_id
             LEFT JOIN albums a ON a.id = t.album_id
             WHERE pi.playlist_id = $1
             ORDER BY pi.position ASC, pi.added_at ASC"#,
            playlist_id
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("playlist.get_tracks"))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn add_collaborator(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        new_collaborator_id: &str,
    ) -> Result<(), AppError> {
        // Verify ownership
        let owner =
            sqlx::query_scalar!("SELECT owner_id FROM playlists WHERE id = $1", playlist_id)
                .fetch_optional(&self.db.0)
                .await
                .map_err(db_err!("playlist.add_collaborator.check_owner"))?;

        match owner {
            None => {
                return Err(AppError::Playlist {
                    kind: PlaylistErrorKind::NotFound,
                    detail: format!("playlist {playlist_id} not found"),
                });
            }
            Some(ref o) if o != owner_id => {
                return Err(AppError::Playlist {
                    kind: PlaylistErrorKind::Forbidden,
                    detail: "only the playlist owner can invite collaborators".to_string(),
                });
            }
            _ => {}
        }

        // Check collaborator limit
        let count: i64 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM playlist_collaborators WHERE playlist_id = $1"#,
            playlist_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("playlist.add_collaborator.count"))?;

        if count >= PLAYLIST_COLLABORATOR_LIMIT as i64 {
            return Err(AppError::Playlist {
                kind: PlaylistErrorKind::CollaboratorLimit,
                detail: format!(
                    "playlist {playlist_id} has reached the {PLAYLIST_COLLABORATOR_LIMIT} collaborator limit"
                ),
            });
        }

        sqlx::query!(
            "INSERT INTO playlist_collaborators (playlist_id, user_id, added_by)
             VALUES ($1, $2, $3)
             ON CONFLICT (playlist_id, user_id) DO NOTHING",
            playlist_id,
            new_collaborator_id,
            owner_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.add_collaborator"))?;

        Ok(())
    }

    async fn remove_collaborator(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        collaborator_id: &str,
    ) -> Result<(), AppError> {
        // Verify ownership
        let owner =
            sqlx::query_scalar!("SELECT owner_id FROM playlists WHERE id = $1", playlist_id)
                .fetch_optional(&self.db.0)
                .await
                .map_err(db_err!("playlist.remove_collaborator.check_owner"))?;

        match owner {
            None => {
                return Err(AppError::Playlist {
                    kind: PlaylistErrorKind::NotFound,
                    detail: format!("playlist {playlist_id} not found"),
                });
            }
            Some(ref o) if o != owner_id => {
                return Err(AppError::Playlist {
                    kind: PlaylistErrorKind::Forbidden,
                    detail: "only the playlist owner can remove collaborators".to_string(),
                });
            }
            _ => {}
        }

        sqlx::query!(
            "DELETE FROM playlist_collaborators WHERE playlist_id = $1 AND user_id = $2",
            playlist_id,
            collaborator_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("playlist.remove_collaborator"))?;

        // Note: tracks added by the removed collaborator intentionally remain.
        Ok(())
    }

    async fn list_collaborators(
        &self,
        playlist_id: Uuid,
        requesting_user: &str,
    ) -> Result<Vec<String>, AppError> {
        self.check_read_access(playlist_id, requesting_user).await?;

        let user_ids: Vec<String> = sqlx::query_scalar!(
            "SELECT user_id FROM playlist_collaborators WHERE playlist_id = $1 ORDER BY added_at ASC",
            playlist_id
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("playlist.list_collaborators"))?;

        Ok(user_ids)
    }
}

// ── Helper row types ─────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct PlaylistItemRow {
    id: Uuid,
    playlist_id: Uuid,
    track_id: Uuid,
    position: i32,
    added_by: String,
    added_at: chrono::DateTime<chrono::Utc>,
}

impl From<PlaylistItemRow> for PlaylistItem {
    fn from(r: PlaylistItemRow) -> Self {
        Self {
            id: r.id,
            playlist_id: r.playlist_id,
            track_id: r.track_id,
            position: r.position,
            added_by: r.added_by,
            added_at: r.added_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PlaylistSummaryRow {
    id: Uuid,
    name: String,
    owner_id: String,
    visibility: String,
    track_count: i64,
}

impl From<PlaylistSummaryRow> for PlaylistSummary {
    fn from(r: PlaylistSummaryRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            owner_id: r.owner_id,
            visibility: r.visibility.parse().unwrap_or(PlaylistVisibility::Private),
            track_count: r.track_count,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PlaylistItemWithTrack {
    item_id: Uuid,
    playlist_id: Uuid,
    track_id: Uuid,
    position: i32,
    added_by: String,
    added_at: chrono::DateTime<chrono::Utc>,
    title: String,
    artist_display: Option<String>,
    album_title: Option<String>,
    album_id: Option<Uuid>,
    duration_ms: Option<i64>,
    blob_location: Option<String>,
}

#[derive(sqlx::FromRow)]
struct TrackSummaryRow {
    id: Uuid,
    title: String,
    artist_display: Option<String>,
    album_title: Option<String>,
    album_id: Option<Uuid>,
    duration_ms: Option<i64>,
    blob_location: Option<String>,
}

impl From<TrackSummaryRow> for TrackSummary {
    fn from(r: TrackSummaryRow) -> Self {
        Self {
            id: r.id,
            title: r.title,
            artist_display: r.artist_display,
            album_title: r.album_title,
            album_id: r.album_id,
            duration_ms: r.duration_ms,
            blob_location: r.blob_location,
        }
    }
}
