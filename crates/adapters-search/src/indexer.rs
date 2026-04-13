use std::collections::HashSet;

use sqlx::PgPool;
use tantivy::{IndexWriter, TantivyDocument};
use uuid::Uuid;

use application::{AppError, SearchErrorKind};
use domain::search::SearchResult;

use crate::schema::MusicSchema;

/// Number of documents to buffer before flushing to disk during a full
/// rebuild. Higher values use more memory but produce fewer segments,
/// leading to faster post-rebuild search. 500 is safe for 50k tracks;
/// increase to 2000 if rebuild memory usage is acceptable on the host.
/// Pass 1.1 TODO: make this configurable via SearchConfig.
const COMMIT_BATCH_SIZE: usize = 2000;

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
    pub source: String,
    pub youtube_video_id: Option<String>,
    pub youtube_uploader: Option<String>,
    pub duration_ms: Option<i64>,
    pub play_count: Option<i64>,
}

// ─── Unified document builder trait ────────────────────────────────────
// S1: All types that can be indexed into Tantivy implement this trait.
// Ensures startup rebuild, incremental reindex, and live search-result
// inserts all produce identical field sets.

pub trait ToSearchDoc {
    fn to_search_doc(&self, s: &MusicSchema) -> TantivyDocument;
}

/// Shared helper: populate the common "transient YouTube" fields that
/// both `YoutubeSearchCacheRow` and `SearchResult` share.
fn build_transient_doc(
    s: &MusicSchema,
    title: &str,
    uploader: Option<&str>,
    source: &str,
    video_id: Option<&str>,
    duration_ms: Option<i64>,
) -> TantivyDocument {
    let mut doc = TantivyDocument::default();
    doc.add_text(s.track_id, ""); // No track_id for transient docs
    doc.add_text(s.title, title);

    if let Some(up) = uploader {
        doc.add_text(s.artist, up);
        doc.add_text(s.uploader, up);
    }

    doc.add_text(s.source, source);
    if let Some(vid) = video_id {
        doc.add_text(s.youtube_video_id, vid);
    }

    // All FAST fields must be present — Tantivy FAST columns are non-optional
    doc.add_u64(s.year, 0);
    doc.add_u64(s.bpm, 0);
    doc.add_u64(s.duration_ms, duration_ms.map_or(0, |d| d.cast_unsigned()));
    doc.add_u64(s.play_count, 0);

    doc
}

impl ToSearchDoc for domain::youtube::YoutubeSearchCacheRow {
    fn to_search_doc(&self, s: &MusicSchema) -> TantivyDocument {
        build_transient_doc(
            s,
            &self.title,
            self.uploader.as_deref(),
            "youtube_search",
            Some(&self.video_id),
            self.duration_ms.map(i64::from),
        )
    }
}

impl ToSearchDoc for SearchResult {
    fn to_search_doc(&self, s: &MusicSchema) -> TantivyDocument {
        build_transient_doc(
            s,
            &self.title,
            self.uploader.as_deref().or(self.artist_display.as_deref()),
            &self.source,
            self.youtube_video_id.as_deref(),
            self.duration_ms,
        )
    }
}

impl ToSearchDoc for TrackRow {
    fn to_search_doc(&self, s: &MusicSchema) -> TantivyDocument {
        let mut doc = TantivyDocument::default();

        doc.add_text(s.track_id, self.track_id.to_string());
        doc.add_text(s.title, &self.title);

        // Artists — deduplicated, each value indexed as an independent token stream.
        let mut seen: HashSet<&str> = HashSet::with_capacity(8);
        for name in self
            .primary_artists
            .iter()
            .chain(self.artist_sort_names.iter())
            .chain(self.featured_artists.iter())
        {
            if seen.insert(name.as_str()) {
                doc.add_text(s.artist, name);
            }
        }
        // Fallback: artist_display for tracks without full artist rows yet.
        if self.primary_artists.is_empty()
            && let Some(ref ad) = self.artist_display
            && seen.insert(ad.as_str())
        {
            doc.add_text(s.artist, ad);
        }

        if let Some(ref album) = self.album_title {
            doc.add_text(s.album, album);
        }

        // Genres: merge track + album, deduplicated
        let mut genres: HashSet<&str> =
            HashSet::with_capacity(self.track_genres.len() + self.album_genres.len());
        genres.extend(self.track_genres.iter().map(String::as_str));
        genres.extend(self.album_genres.iter().map(String::as_str));
        for g in genres {
            doc.add_text(s.genre, g);
        }

        for name in &self.composers {
            doc.add_text(s.composer, name);
        }
        for name in &self.lyricists {
            doc.add_text(s.lyricist, name);
        }

        // NOTE: year=0 means "unknown". When year-range filtering is added,
        // this field must be reconsidered.
        doc.add_u64(s.year, self.year.map_or(0, |y| y as u64));
        doc.add_u64(s.bpm, self.bpm.map_or(0, |b| b as u64));

        doc.add_text(s.source, &self.source);

        if let Some(ref vid) = self.youtube_video_id {
            doc.add_text(s.youtube_video_id, vid);
        }
        if let Some(ref up) = self.youtube_uploader {
            doc.add_text(s.uploader, up);
        }
        doc.add_u64(
            s.duration_ms,
            self.duration_ms.map_or(0, |b| b.cast_unsigned()),
        );
        doc.add_u64(
            s.play_count,
            self.play_count.map_or(0, |b| b.cast_unsigned()),
        );

        doc
    }
}

/// Fetch data from the DB to completely rebuild the index.
/// Pulled out to avoid holding Tantivy lock during I/O.
pub async fn fetch_rebuild_data(
    pool: &PgPool,
) -> Result<(Vec<TrackRow>, Vec<domain::youtube::YoutubeSearchCacheRow>), AppError> {
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
            )                                                        AS "featured_artists!: Vec<String>",
            t.source                                                 AS "source!",
            t.youtube_video_id                                       AS "youtube_video_id?",
            t.youtube_uploader                                       AS "youtube_uploader?",
            t.duration_ms                                            AS "duration_ms?",
            COALESCE(SUM(gts.play_count), 0)                         AS "play_count: i64"
        FROM tracks t
        LEFT JOIN albums            al ON al.id       = t.album_id
        LEFT JOIN track_artists     ta ON ta.track_id = t.id
        LEFT JOIN artists           ar ON ar.id       = ta.artist_id
        LEFT JOIN guild_track_stats gts ON gts.track_id = t.id
        GROUP BY t.id, al.title, al.genres
        "#
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Search {
        kind: SearchErrorKind::RebuildFailed,
        detail: e.to_string(),
    })?;

    let cache_rows: Vec<domain::youtube::YoutubeSearchCacheRow> = sqlx::query_as!(
        domain::youtube::YoutubeSearchCacheRow,
        r#"
        SELECT
            id, video_id, title, uploader, channel_id, duration_ms, thumbnail_url, query, track_id, created_at, last_seen_at
        FROM youtube_search_cache
        WHERE track_id IS NULL AND video_id NOT IN (
            SELECT youtube_video_id FROM tracks WHERE youtube_video_id IS NOT NULL
        )
        "#
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Search {
        kind: SearchErrorKind::RebuildFailed,
        detail: e.to_string(),
    })?;

    Ok((rows, cache_rows))
}

/// Full rebuild. Clears the index and reindexes all passed tracks.
pub fn execute_rebuild(
    writer: &mut IndexWriter,
    schema: &MusicSchema,
    rows: &[TrackRow],
    cache_rows: &[domain::youtube::YoutubeSearchCacheRow],
) -> Result<usize, AppError> {
    writer
        .delete_all_documents()
        .map_err(|e: tantivy::TantivyError| write_err(&e))?;

    let total = rows.len() + cache_rows.len();
    let mut i = 0;

    for row in rows {
        writer
            .add_document(row.to_search_doc(schema))
            .map_err(|e: tantivy::TantivyError| write_err(&e))?;

        i += 1;
        if i % COMMIT_BATCH_SIZE == 0 {
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

    for row in cache_rows {
        writer
            .add_document(row.to_search_doc(schema))
            .map_err(|e: tantivy::TantivyError| write_err(&e))?;

        i += 1;
        if i % COMMIT_BATCH_SIZE == 0 {
            writer
                .commit()
                .map_err(|e: tantivy::TantivyError| write_err(&e))?;
            tracing::debug!(
                indexed = i,
                total = total,
                pct = (i * 100) / total,
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

pub async fn fetch_single_track(
    pool: &PgPool,
    track_id: Uuid,
) -> Result<Option<TrackRow>, AppError> {
    sqlx::query_as!(
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
            )                                                        AS "featured_artists!: Vec<String>",
            t.source                                                 AS "source!",
            t.youtube_video_id                                       AS "youtube_video_id?",
            t.youtube_uploader                                       AS "youtube_uploader?",
            t.duration_ms                                            AS "duration_ms?",
            COALESCE(SUM(gts.play_count), 0)                         AS "play_count: i64"
        FROM tracks t
        LEFT JOIN albums            al ON al.id       = t.album_id
        LEFT JOIN track_artists     ta ON ta.track_id = t.id
        LEFT JOIN artists           ar ON ar.id       = ta.artist_id
        LEFT JOIN guild_track_stats gts ON gts.track_id = t.id
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
    })
}

pub fn execute_reindex_track(
    writer: &mut IndexWriter,
    schema: &MusicSchema,
    track_id: Uuid,
    track_row: Option<&TrackRow>,
) -> Result<(), AppError> {
    let id_term = tantivy::Term::from_field_text(schema.track_id, &track_id.to_string());
    writer.delete_term(id_term);

    if let Some(row) = track_row {
        let doc = row.to_search_doc(schema);
        writer
            .add_document(doc)
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
