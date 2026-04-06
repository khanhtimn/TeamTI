use adapters_persistence::repositories::track_repository::PgTrackRepository;
use domain::track::Track;

async fn test(pool: &sqlx::PgPool) {
    let locations: Vec<String> = vec![];
    let tracks = sqlx::query_as!(
        Track,
        r#"
            SELECT id, title, artist_display, album_title, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: _", 
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at,
                analysis_status as "analysis_status: _", 
                analysis_attempts, analysis_locked, analyzed_at
            FROM tracks t
            JOIN UNNEST($1::text[]) AS u(loc) ON t.blob_location = u.loc
        "#,
        &locations
    ).fetch_all(pool).await;
}

async fn test2(pool: &sqlx::PgPool) {
    let track_id = uuid::Uuid::nil();
    let vec = pgvector::Vector::from(vec![0.0f32; 23]);
    sqlx::query!(
        "UPDATE tracks SET analysis_status = 'done', bliss_vector = $2, analyzed_at = NOW() WHERE id = $1",
        track_id,
        vec as pgvector::Vector
    ).execute(pool).await;
}
