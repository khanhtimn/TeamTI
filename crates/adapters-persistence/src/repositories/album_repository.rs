use async_trait::async_trait;
use uuid::Uuid;

use application::AppError;
use application::ports::repository::AlbumRepository;
use domain::Album;

use crate::db::Database;

pub struct PgAlbumRepository {
    db: Database,
}

impl PgAlbumRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AlbumRepository for PgAlbumRepository {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Album>, AppError> {
        let row = sqlx::query_as::<_, Album>(
            r#"
            SELECT id, title, release_year, total_tracks, total_discs,
                   mbid, cover_art_path, created_at
            FROM albums WHERE mbid = $1 LIMIT 1
            "#,
        )
        .bind(mbid)
        .fetch_optional(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn upsert(&self, album: &Album) -> Result<Album, AppError> {
        let row = sqlx::query_as::<_, Album>(
            r#"
            INSERT INTO albums (id, title, release_year, total_tracks, total_discs,
                                mbid, cover_art_path, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (mbid) DO UPDATE
            SET title = EXCLUDED.title,
                release_year = EXCLUDED.release_year,
                total_tracks = EXCLUDED.total_tracks,
                total_discs = EXCLUDED.total_discs,
                cover_art_path = EXCLUDED.cover_art_path
            RETURNING id, title, release_year, total_tracks, total_discs,
                      mbid, cover_art_path, created_at
            "#,
        )
        .bind(album.id)
        .bind(&album.title)
        .bind(album.release_year)
        .bind(album.total_tracks)
        .bind(album.total_discs)
        .bind(&album.mbid)
        .bind(&album.cover_art_path)
        .bind(album.created_at)
        .fetch_one(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn update_cover_art_path(&self, id: Uuid, path: &str) -> Result<(), AppError> {
        sqlx::query("UPDATE albums SET cover_art_path = $1 WHERE id = $2")
            .bind(path)
            .bind(id)
            .execute(&self.db.0)
            .await?;

        Ok(())
    }
}
