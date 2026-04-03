use async_trait::async_trait;

use application::AppError;
use application::ports::repository::ArtistRepository;
use domain::{AlbumArtist, Artist, TrackArtist};

use crate::db::Database;

pub struct PgArtistRepository {
    db: Database,
}

impl PgArtistRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ArtistRepository for PgArtistRepository {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Artist>, AppError> {
        let row = sqlx::query_as::<_, Artist>(
            r#"
            SELECT id, name, sort_name, mbid, country, created_at
            FROM artists WHERE mbid = $1 LIMIT 1
            "#,
        )
        .bind(mbid)
        .fetch_optional(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn upsert(&self, artist: &Artist) -> Result<Artist, AppError> {
        let row = sqlx::query_as::<_, Artist>(
            r#"
            INSERT INTO artists (id, name, sort_name, mbid, country, created_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (mbid) DO UPDATE
            SET name = EXCLUDED.name,
                sort_name = EXCLUDED.sort_name,
                country = EXCLUDED.country
            RETURNING id, name, sort_name, mbid, country, created_at
            "#,
        )
        .bind(artist.id)
        .bind(&artist.name)
        .bind(&artist.sort_name)
        .bind(&artist.mbid)
        .bind(&artist.country)
        .bind(artist.created_at)
        .fetch_one(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn upsert_track_artist(&self, ta: &TrackArtist) -> Result<(), AppError> {
        sqlx::query(
            r#"
            INSERT INTO track_artists (track_id, artist_id, role, position)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (track_id, artist_id, role) DO UPDATE
            SET position = EXCLUDED.position
            "#,
        )
        .bind(ta.track_id)
        .bind(ta.artist_id)
        .bind(&ta.role)
        .bind(ta.position)
        .execute(&self.db.0)
        .await?;

        Ok(())
    }

    async fn upsert_album_artist(&self, aa: &AlbumArtist) -> Result<(), AppError> {
        sqlx::query(
            r#"
            INSERT INTO album_artists (album_id, artist_id, role, position)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (album_id, artist_id) DO UPDATE
            SET role = EXCLUDED.role,
                position = EXCLUDED.position
            "#,
        )
        .bind(aa.album_id)
        .bind(aa.artist_id)
        .bind(&aa.role)
        .bind(aa.position)
        .execute(&self.db.0)
        .await?;

        Ok(())
    }
}
