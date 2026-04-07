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
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AlbumRepository for PgAlbumRepository {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Album>, AppError> {
        let row = sqlx::query_as!(
            Album,
            r#"
            SELECT id, title, release_year, release_date, total_tracks, total_discs,
                   mbid, record_label, upc_barcode, genres, cover_art_path, created_at
            FROM albums WHERE mbid = $1 LIMIT 1
            "#,
            mbid,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("album.find_by_mbid"))?;

        Ok(row)
    }

    async fn upsert(&self, album: &Album) -> Result<Album, AppError> {
        let row = sqlx::query_as!(
            Album,
            r#"
            INSERT INTO albums (id, title, release_year, release_date, total_tracks, total_discs,
                                mbid, record_label, upc_barcode, genres, cover_art_path, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (mbid) DO UPDATE SET
                title        = EXCLUDED.title,
                release_year = COALESCE(EXCLUDED.release_year, albums.release_year),
                release_date = COALESCE(EXCLUDED.release_date, albums.release_date),
                total_tracks = COALESCE(EXCLUDED.total_tracks, albums.total_tracks),
                record_label = COALESCE(EXCLUDED.record_label, albums.record_label),
                upc_barcode  = COALESCE(EXCLUDED.upc_barcode, albums.upc_barcode),
                genres       = COALESCE(EXCLUDED.genres, albums.genres)
            RETURNING id, title, release_year, release_date, total_tracks, total_discs,
                      mbid, record_label, upc_barcode, genres, cover_art_path, created_at
            "#,
            album.id,
            album.title,
            album.release_year,
            album.release_date as Option<chrono::NaiveDate>,
            album.total_tracks,
            album.total_discs,
            album.mbid,
            album.record_label,
            album.upc_barcode,
            album.genres.as_deref() as Option<&[String]>,
            album.cover_art_path,
            album.created_at,
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(crate::db_err!("album.upsert"))?;

        Ok(row)
    }

    async fn update_cover_art_path(&self, id: Uuid, path: &str) -> Result<(), AppError> {
        sqlx::query!(
            "UPDATE albums SET cover_art_path = $1 WHERE id = $2",
            path,
            id,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("album.update_cover_art_path"))?;

        Ok(())
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<Album>, AppError> {
        let row = sqlx::query_as!(
            Album,
            r#"
            SELECT id, title, release_year, release_date, total_tracks, total_discs,
                   mbid, record_label, upc_barcode, genres, cover_art_path, created_at
            FROM albums WHERE id = $1 LIMIT 1
            "#,
            id,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("album.find_by_id"))?;

        Ok(row)
    }
}
