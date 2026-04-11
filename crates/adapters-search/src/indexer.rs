use std::collections::HashSet;

use sqlx::PgPool;
use tantivy::{IndexWriter, TantivyDocument};
use uuid::Uuid;

use application::{AppError, SearchErrorKind};

use crate::schema::MusicSchema;

/// Number of documents to buffer before flushing to disk during a full
/// rebuild. Higher values use more memory but produce fewer segments,
/// leading to faster post-rebuild search. 500 is safe for 50k tracks;
/// increase to 2000 if rebuild memory usage is acceptable on the host.
/// Pass 1.1 TODO: make this configurable via SearchConfig.
const COMMIT_BATCH_SIZE: usize = 500;

/// Row returned by the denormalization query.
/// All array columns are COALESCE'd to empty arrays — never null.
#[derive(Debug)]
pub struct TrackRow {
    pub track_id: Uuid,
    pub title: String,
    pub artist_display: Option<String>,
    pub year: Option<i32>,
    pub bpm: Option<i32>,
    pub track_genres: Vec<String>,
    pub album_title: Option<String>,
    pub album_genres: Vec<String>,
    pub primary_artists: Vec<String>,
    pub artist_sort_names: Vec<String>,
    pub composers: Vec<String>,
    pub lyricists: Vec<String>,
    pub featured_artists: Vec<String>,
}

/// Build a Tantivy document from a denormalized database row.
pub fn build_document(row: &TrackRow, s: &MusicSchema) -> TantivyDocument {
    let mut doc = TantivyDocument::default();

    doc.add_text(s.track_id, row.track_id.to_string());
    doc.add_text(s.title, &row.title);

    // Artists — deduplicated, each value indexed as an independent token stream.
    // Insertion order: primary name, sort_name, featuring.
    // Typical track has 1-3 primary artists + 1 sort name + 0-2 featuring = ~5
    let mut seen: HashSet<&str> = HashSet::with_capacity(8);
    for name in row
        .primary_artists
        .iter()
        .chain(row.artist_sort_names.iter())
        .chain(row.featured_artists.iter())
    {
        if seen.insert(name.as_str()) {
            doc.add_text(s.artist, name);
        }
    }
    // Fallback: artist_display for tracks without full artist rows yet.
    if row.primary_artists.is_empty()
        && let Some(ref ad) = row.artist_display
        && seen.insert(ad.as_str())
    {
        doc.add_text(s.artist, ad);
    }

    if let Some(ref album) = row.album_title {
        doc.add_text(s.album, album);
    }

    // Genres: merge track + album, deduplicated
    let mut genres: HashSet<&str> =
        HashSet::with_capacity(row.track_genres.len() + row.album_genres.len());
    genres.extend(row.track_genres.iter().map(String::as_str));
    genres.extend(row.album_genres.iter().map(String::as_str));
    for g in genres {
        doc.add_text(s.genre, g);
    }

    for name in &row.composers {
        doc.add_text(s.composer, name);
    }
    for name in &row.lyricists {
        doc.add_text(s.lyricist, name);
    }

    // NOTE: year=0 means "unknown". When year-range filtering is added,
    // this field must be reconsidered. Options:
    //   a) Separate has_year: u64 (0/1) fast field
    //   b) Use i64::MIN as sentinel
    // Do not use year=0 as a filter target.
    doc.add_u64(s.year, row.year.map_or(0, |y| y as u64));
    doc.add_u64(s.bpm, row.bpm.map_or(0, |b| b as u64));

    doc
}

/// Full rebuild. Clears the index and reindexes all 'done' tracks.
/// Called at startup before any workers are spawned.
/// Returns the number of documents indexed.
pub async fn rebuild_index(
    writer: &mut IndexWriter,
    pool: &PgPool,
    schema: &MusicSchema,
) -> Result<usize, AppError> {
    writer
        .delete_all_documents()
        .map_err(|e: tantivy::TantivyError| write_err(&e))?;

    // Use runtime query_as instead of query_as! because the denormalized
    // query with array_agg is hard to express with compile-time checking.
    // sqlx::query_as! can't map COALESCE'd array_agg columns cleanly.
    let rows: Vec<TrackRow> = sqlx::query_as!(
        TrackRow,
        r#"
        SELECT
            t.id                                                     AS "track_id!: Uuid",
            t.title                                                  AS "title!",
            t.artist_display,
            t.year,
            t.bpm,
            COALESCE(t.genres, '{}'::text[])                         AS "track_genres!: Vec<String>",
            al.title                                                 AS "album_title?",
            COALESCE(al.genres, '{}'::text[])                        AS "album_genres!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'primary'),
                '{}'::text[]
            )                                                        AS "primary_artists!: Vec<String>",
            COALESCE(
                array_agg(ar.sort_name) FILTER (WHERE ta.role = 'primary'),
                '{}'::text[]
            )                                                        AS "artist_sort_names!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'composer'),
                '{}'::text[]
            )                                                        AS "composers!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'lyricist'),
                '{}'::text[]
            )                                                        AS "lyricists!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'featuring'),
                '{}'::text[]
            )                                                        AS "featured_artists!: Vec<String>"
        FROM tracks t
        LEFT JOIN albums        al ON al.id       = t.album_id
        LEFT JOIN track_artists ta ON ta.track_id = t.id
        LEFT JOIN artists       ar ON ar.id       = ta.artist_id
        GROUP BY t.id, al.title, al.genres
        "#
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Search {
        kind: SearchErrorKind::RebuildFailed,
        detail: e.to_string(),
    })?;

    let total = rows.len();

    for (i, row) in rows.iter().enumerate() {
        writer
            .add_document(build_document(row, schema))
            .map_err(|e: tantivy::TantivyError| write_err(&e))?;

        if (i + 1) % COMMIT_BATCH_SIZE == 0 {
            writer
                .commit()
                .map_err(|e: tantivy::TantivyError| write_err(&e))?;
            tracing::debug!(
                indexed = i + 1,
                total = total,
                pct = ((i + 1) * 100) / total,
                operation = "search.rebuild_progress",
                "index rebuild in progress"
            );
        }
    }

    writer
        .commit()
        .map_err(|e: tantivy::TantivyError| write_err(&e))?;

    tracing::info!(
        documents = total,
        operation = "search.index_rebuilt",
        "Tantivy index rebuilt from PostgreSQL"
    );

    Ok(total)
}

/// Incremental update for a single track.
/// Delete-then-insert: Tantivy documents are immutable once written.
/// Called by the post-enrichment hook in TagWriterWorker.
pub async fn reindex_track(
    writer: &mut IndexWriter,
    pool: &PgPool,
    schema: &MusicSchema,
    track_id: Uuid,
) -> Result<(), AppError> {
    // Delete any existing document with this track_id
    let id_term = tantivy::Term::from_field_text(schema.track_id, &track_id.to_string());
    writer.delete_term(id_term);

    let row = sqlx::query_as!(
        TrackRow,
        r#"
        SELECT
            t.id                                                     AS "track_id!: Uuid",
            t.title                                                  AS "title!",
            t.artist_display,
            t.year,
            t.bpm,
            COALESCE(t.genres, '{}'::text[])                         AS "track_genres!: Vec<String>",
            al.title                                                 AS "album_title?",
            COALESCE(al.genres, '{}'::text[])                        AS "album_genres!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'primary'),
                '{}'::text[]
            )                                                        AS "primary_artists!: Vec<String>",
            COALESCE(
                array_agg(ar.sort_name) FILTER (WHERE ta.role = 'primary'),
                '{}'::text[]
            )                                                        AS "artist_sort_names!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'composer'),
                '{}'::text[]
            )                                                        AS "composers!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'lyricist'),
                '{}'::text[]
            )                                                        AS "lyricists!: Vec<String>",
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'featuring'),
                '{}'::text[]
            )                                                        AS "featured_artists!: Vec<String>"
        FROM tracks t
        LEFT JOIN albums        al ON al.id       = t.album_id
        LEFT JOIN track_artists ta ON ta.track_id = t.id
        LEFT JOIN artists       ar ON ar.id       = ta.artist_id
        WHERE t.id = $1
        GROUP BY t.id, al.title, al.genres
        "#,
        track_id
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Search {
        kind: SearchErrorKind::ReadFailed,
        detail: e.to_string(),
    })?;

    if let Some(row) = row {
        writer
            .add_document(build_document(&row, schema))
            .map_err(|e: tantivy::TantivyError| write_err(&e))?;
    }

    writer
        .commit()
        .map_err(|e: tantivy::TantivyError| write_err(&e))?;

    tracing::debug!(
        %track_id,
        operation = "search.track_reindexed",
        "track reindexed in Tantivy"
    );

    Ok(())
}

fn write_err(e: &tantivy::TantivyError) -> AppError {
    AppError::Search {
        kind: SearchErrorKind::WriteFailed,
        detail: e.to_string(),
    }
}
