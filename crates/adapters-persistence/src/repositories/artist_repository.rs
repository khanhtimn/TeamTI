use async_trait::async_trait;

use application::AppError;
use application::ports::repository::ArtistRepository;
use domain::{AlbumArtist, Artist, ArtistRole, TrackArtist};

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
        let row = sqlx::query_as!(
            Artist,
            r#"
            SELECT id, name, sort_name, mbid, country, created_at
            FROM artists WHERE mbid = $1 LIMIT 1
            "#,
            mbid,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("artist.find_by_mbid"))?;

        Ok(row)
    }

    async fn find_by_track_id(
        &self,
        track_id: uuid::Uuid,
    ) -> Result<Vec<(TrackArtist, Artist)>, AppError> {
        let rows = sqlx::query!(
            r#"
            SELECT
                ta.track_id, ta.artist_id, ta.role as "role: ArtistRole", ta.position,
                a.id, a.name, a.sort_name, a.mbid, a.country, a.created_at
            FROM track_artists ta
            JOIN artists a ON a.id = ta.artist_id
            WHERE ta.track_id = $1
            ORDER BY ta.position ASC
            "#,
            track_id,
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(crate::db_err!("artist.find_by_track_id"))?;

        let result = rows
            .into_iter()
            .map(|r| {
                let ta = TrackArtist {
                    track_id: r.track_id,
                    artist_id: r.artist_id,
                    role: r.role,
                    position: r.position,
                };
                let a = Artist {
                    id: r.id,
                    name: r.name,
                    sort_name: r.sort_name,
                    mbid: r.mbid,
                    country: r.country,
                    created_at: r.created_at,
                };
                (ta, a)
            })
            .collect();

        Ok(result)
    }

    async fn upsert(&self, artist: &Artist) -> Result<Artist, AppError> {
        let row = sqlx::query_as!(
            Artist,
            r#"
            INSERT INTO artists (id, name, sort_name, mbid, country, created_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (mbid) DO UPDATE SET
                name      = EXCLUDED.name,
                sort_name = EXCLUDED.sort_name,
                country   = COALESCE(EXCLUDED.country, artists.country)
            RETURNING id, name, sort_name, mbid, country, created_at
            "#,
            artist.id,
            artist.name,
            artist.sort_name,
            artist.mbid,
            artist.country,
            artist.created_at,
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(crate::db_err!("artist.upsert"))?;

        Ok(row)
    }

    async fn upsert_track_artist(&self, ta: &TrackArtist) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            INSERT INTO track_artists (track_id, artist_id, role, position)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (track_id, artist_id, role) DO UPDATE
            SET position = EXCLUDED.position
            "#,
            ta.track_id,
            ta.artist_id,
            &ta.role as &ArtistRole,
            ta.position,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("artist.upsert_track_artist"))?;

        Ok(())
    }

    async fn upsert_album_artist(&self, aa: &AlbumArtist) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            INSERT INTO album_artists (album_id, artist_id, role, position)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (album_id, artist_id) DO UPDATE
            SET role = EXCLUDED.role,
                position = EXCLUDED.position
            "#,
            aa.album_id,
            aa.artist_id,
            &aa.role as &ArtistRole,
            aa.position,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("artist.upsert_album_artist"))?;

        Ok(())
    }
}
