# TeamTI v3 — Pass 1 Prompt
## Search Migration: PostgreSQL FTS → Tantivy 0.26

> Implementation pass. Apply all changes described here.
> All API references target tantivy = "0.26". Before writing any code,
> open https://docs.rs/tantivy/0.26.0 and verify feature flags, struct name,
> module path, and method signature against this document. Where the
> 0.26 API diverges from what is written here, follow 0.26.

---

### Scope

**In scope — implement in this pass:**
- Inline modifications to existing migration files (no new migration file)
- `crates/adapters-search` with `TantivySearchAdapter`
- Tantivy tokenizer pipeline: `SimpleTokenizer → LowerCaser → AsciiFoldingFilter`
- Per-token `FuzzyTermQuery` (distance=1) for all interior tokens
- Prefix + fuzzy (`FuzzyTermQuery::new_prefix`) for the final token
- Per-field `BoostQuery` weighting across all relational fields
- Startup index rebuild + incremental post-enrichment reindex
- `/play` autocomplete working end-to-end via Tantivy
- `AppError::Search` variant per Pass 4.5 conventions

**Deferred to a later pass:**
- Listen history scoring (`listen_count` fast-field boost)
- Year / BPM range filtering
- Faceted search
- Boost value fine-tuning against a real library
- Index merge policy tuning

---

### Step 1 — Modify Existing Migration Files

These are v3 schema files. The v3 database has not been applied yet.
Modify them in-place. After all edits, reset and re-apply cleanly:

```bash
cargo sqlx database drop && cargo sqlx database create
cargo sqlx migrate run
cargo sqlx prepare --workspace   # must pass with zero errors
```

---

#### `0001_extensions.sql` — remove pg_trgm and music_simple

Remove the `pg_trgm` extension and the `music_simple` FTS configuration
block entirely. Keep `unaccent`, `immutable_unaccent()`, and their
comments — the rebuild SQL query still uses them for normalization.

**Before:**
```sql
CREATE EXTENSION IF NOT EXISTS unaccent;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE OR REPLACE FUNCTION immutable_unaccent(text) ...

DO $$ ... CREATE TEXT SEARCH CONFIGURATION music_simple ... $$;
```

**After:**
```sql
-- Extensions and custom functions required before any table creation.
-- This migration must run first and must be idempotent.
--
-- NOTE (v3): pg_trgm and the music_simple FTS configuration have been
-- removed. Full-text search is now handled by the embedded Tantivy index
-- (adapters-search). The unaccent extension is retained because
-- immutable_unaccent() is used by the Tantivy rebuild query.

CREATE EXTENSION IF NOT EXISTS unaccent;

-- Standard unaccent() is STABLE, not IMMUTABLE.
-- Generated columns require IMMUTABLE functions.
CREATE OR REPLACE FUNCTION immutable_unaccent(text)
RETURNS text LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT unaccent($1) $$;
```

---

#### `0002_core_tables-2.sql` — remove generated search columns from tracks

Remove both generated column definitions from the `CREATE TABLE tracks`
statement. Remove `search_text` and `search_vector` entirely.

**Remove these two blocks from the tracks table definition:**
```sql
-- REMOVE THIS:
search_text TEXT GENERATED ALWAYS AS (
    lower(immutable_unaccent(
        normalize(coalesce(title, ''), NFC) || ' ' ||
        normalize(coalesce(artist_display, ''), NFC)
    ))
) STORED,

-- AND REMOVE THIS:
search_vector tsvector GENERATED ALWAYS AS (
    to_tsvector('music_simple',
        normalize(coalesce(title, ''), NFC) || ' ' ||
        normalize(coalesce(artist_display, ''), NFC)
    )
) STORED
```

Add a comment above the closing `)` of the tracks table:
```sql
-- search_text and search_vector generated columns removed in v3.
-- Full-text search is handled by adapters-search (Tantivy).
```

---

#### `0004_indexes-4.sql` — remove search GIN indexes

Remove the two search GIN index definitions. Keep all other indexes.

**Remove:**
```sql
-- REMOVE:
CREATE INDEX IF NOT EXISTS idx_tracks_search_vector
ON tracks USING GIN(search_vector);

-- REMOVE:
CREATE INDEX IF NOT EXISTS idx_tracks_search_text
ON tracks USING GIN(search_text gin_trgm_ops);
```

Add a comment in their place:
```sql
-- idx_tracks_search_vector and idx_tracks_search_text removed in v3.
-- GIN indexes are no longer needed; Tantivy owns the search index.
```

---

### Step 2 — AppError Integration

In `crates/application/src/error.rs`, add the `Search` variant following
the existing pattern exactly.

```rust
// In AppError enum:
#[error("search error ({kind}): {detail}")]
Search {
    kind:   SearchErrorKind,
    detail: String,
},

// Alongside AppError, in the same file:
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SearchErrorKind {
    #[error("index not initialized")]  NotInitialized,
    #[error("index open failed")]      OpenFailed,
    #[error("index write failed")]     WriteFailed,
    #[error("index read failed")]      ReadFailed,
    #[error("index rebuild failed")]   RebuildFailed,
    #[error("malformed document")]     MalformedDocument,
}
```

In `kind_str()`:
```rust
AppError::Search { kind, .. } => match kind {
    SearchErrorKind::NotInitialized  => "search.not_initialized",
    SearchErrorKind::OpenFailed      => "search.open_failed",
    SearchErrorKind::WriteFailed     => "search.write_failed",
    SearchErrorKind::ReadFailed      => "search.read_failed",
    SearchErrorKind::RebuildFailed   => "search.rebuild_failed",
    SearchErrorKind::MalformedDocument => "search.malformed_document",
},
```

In `impl Retryable for AppError`:
```rust
AppError::Search { kind, .. } => matches!(
    kind,
    SearchErrorKind::WriteFailed | SearchErrorKind::ReadFailed
),
```

---

### Step 3 — Crate: `adapters-search`

```
crates/adapters-search/
├── Cargo.toml
└── src/
    ├── lib.rs          TantivySearchAdapter — public API + port impl
    ├── schema.rs       MusicSchema — all field handles
    ├── tokenizer.rs    register_music_tokenizer(), tokenize_query()
    ├── indexer.rs      rebuild_index(), reindex_track(), build_document()
    └── searcher.rs     MusicSearcher — query building + execution
```

Register as a workspace member in the root `Cargo.toml`.

---

#### `Cargo.toml`

```toml
[package]
name    = "adapters-search"
version = "0.1.0"
edition = "2021"

[dependencies]
application   = { path = "../application" }
domain        = { path = "../domain" }
shared-config = { path = "../shared-config" }

tantivy     = { workspace = true }
tokio       = { workspace = true, features = ["rt", "sync"] }
sqlx        = { workspace = true, features = ["postgres", "uuid"] }
uuid        = { workspace = true, features = ["v4"] }
tracing     = { workspace = true }
thiserror   = { workspace = true }
async-trait = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

In the workspace `Cargo.toml`, add:
```toml
tantivy = { version = "0.26", default-features = true }
```

---

#### `src/schema.rs`

```rust
use tantivy::schema::{
    IndexRecordOption, NumericOptions, Schema, SchemaBuilder,
    TextFieldIndexing, TextOptions, FAST, STORED, STRING,
};
use tantivy::schema::Field;

/// All field handles for the music index, derived from a single Schema.
/// Constructed once at index open/creation and shared via Arc.
#[derive(Clone, Debug)]
pub struct MusicSchema {
    pub schema:   Schema,

    // ── Full-text fields ──────────────────────────────────────────
    // title and artist: WithFreqsAndPositions enables phrase queries (Pass 1.1).
    // album: WithFreqs is sufficient — phrase order matters less.
    // genre, composer, lyricist: WithFreqs, NOT stored (not shown in results).
    pub title:    Field,   // music tokenizer · WithFreqsAndPositions · STORED
    pub artist:   Field,   // music tokenizer · WithFreqsAndPositions · STORED
    pub album:    Field,   // music tokenizer · WithFreqs              · STORED
    pub genre:    Field,   // music tokenizer · WithFreqs              (not stored)
    pub composer: Field,   // music tokenizer · WithFreqs              (not stored)
    pub lyricist: Field,   // music tokenizer · WithFreqs              (not stored)

    // ── Identifier ───────────────────────────────────────────────
    // STRING = raw token (no tokenization). STORED = returned with results.
    pub track_id: Field,   // STRING · STORED

    // ── Fast fields (scoring / filtering, not text-matched) ──────
    // Reserved for Pass 1.1 listen-history scoring and year/BPM filters.
    pub year:     Field,   // u64 · FAST
    pub bpm:      Field,   // u64 · FAST
}

impl MusicSchema {
    pub fn build() -> Self {
        let mut b = SchemaBuilder::new();

        let with_positions = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("music")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();

        let with_freqs_stored = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("music")
                    .set_index_option(IndexRecordOption::WithFreqs),
            )
            .set_stored();

        let with_freqs = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("music")
                    .set_index_option(IndexRecordOption::WithFreqs),
            );
        // NOT stored — genre/composer/lyricist are not shown in autocomplete

        let fast_u64 = NumericOptions::default().set_fast();

        let title    = b.add_text_field("title",    with_positions.clone());
        let artist   = b.add_text_field("artist",   with_positions.clone());
        let album    = b.add_text_field("album",    with_freqs_stored.clone());
        let genre    = b.add_text_field("genre",    with_freqs.clone());
        let composer = b.add_text_field("composer", with_freqs.clone());
        let lyricist = b.add_text_field("lyricist", with_freqs.clone());
        let track_id = b.add_text_field("track_id", STRING | STORED);
        let year     = b.add_u64_field("year",      fast_u64.clone());
        let bpm      = b.add_u64_field("bpm",       fast_u64);
        let schema   = b.build();

        Self { schema, title, artist, album, genre, composer, lyricist, track_id, year, bpm }
    }
}
```

---

#### `src/tokenizer.rs`

Two responsibilities: registering the tokenizer on an index, and applying
that same tokenizer to raw query strings so that `Term` values match the
form stored in the index dictionary.

```rust
use tantivy::{
    Index,
    tokenizer::{AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer},
};

/// Register the "music" tokenizer on the given index.
/// Must be called both when CREATING and when OPENING an existing index.
/// Tantivy does not persist custom tokenizer registrations — they are
/// re-registered in memory on every process start.
pub fn register_music_tokenizer(index: &Index) {
    let tokenizer = build_music_tokenizer();
    index.tokenizers().register("music", tokenizer);
}

pub fn build_music_tokenizer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        // No stemmer — proper nouns (artist/album/track names) must
        // not be stemmed. "Portishead" != "Port". See v3 design notes.
        // No stop words — "The" is part of "The Beatles", "The National".
        .build()
}

/// Tokenize a raw query string using the same pipeline as the index.
/// Returns tokens in order. The last token is treated specially by the
/// caller (prefix + fuzzy). Interior tokens use fuzzy only.
///
/// This MUST mirror the index-time tokenizer exactly, or FuzzyTermQuery
/// will compute edit distances against the wrong dictionary entries.
pub fn tokenize_query(text: &str) -> Vec<String> {
    let mut tokenizer = build_music_tokenizer();
    let mut stream    = tokenizer.token_stream(text);
    let mut tokens    = Vec::new();

    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }

    tokens
}
```

---

#### `src/searcher.rs`

The query builder is the core of this pass. It constructs a per-token
`FuzzyTermQuery` against all text fields, wraps each in a `BoostQuery`,
and combines them with a `BooleanQuery`.

**Field boost weights** (Pass 1.1 will tune these; the ordering is what
matters here — do not flatten them to equal weights):

| Field | Boost | Rationale |
|-------|-------|-----------|
| title | 4.0 | Primary search intent |
| artist | 3.0 | Second most common; covers all roles + sort_names |
| album | 2.0 | "ok computer" searches are real |
| composer | 1.5 | Classical / jazz use cases |
| genre | 1.0 | Broadest match, lowest specificity |
| lyricist | 0.8 | Least common search intent |

```rust
use tantivy::{
    Index, IndexReader, ReloadPolicy,
    query::{BooleanQuery, BoostQuery, FuzzyTermQuery, AllQuery, Query},
    collector::TopDocs,
    schema::Term,
    TantivyDocument,
};
use application::{AppError, SearchErrorKind};
use domain::TrackSummary;
use crate::{schema::MusicSchema, tokenizer::tokenize_query};

// (field, boost) pairs — evaluated in order, all applied to every token
const FIELD_BOOSTS: &[(fn(&MusicSchema) -> tantivy::schema::Field, f32)] = &[
    (|s: &MusicSchema| s.title,    4.0),
    (|s: &MusicSchema| s.artist,   3.0),
    (|s: &MusicSchema| s.album,    2.0),
    (|s: &MusicSchema| s.composer, 1.5),
    (|s: &MusicSchema| s.genre,    1.0),
    (|s: &MusicSchema| s.lyricist, 0.8),
];
```

Note: The `fn(&MusicSchema) -> Field` accessor pattern avoids storing a
`Vec<(Field, f32)>` while keeping the field list in one place. If the
pattern causes type inference issues, replace with an explicit `Vec`
constructed in `MusicSearcher::new`.

```rust
/// Cloneable — cheap because IndexReader is Arc-backed internally.
#[derive(Clone)]
pub struct MusicSearcher {
    reader: IndexReader,
    schema: MusicSchema,
}

impl MusicSearcher {
    pub fn new(index: &Index, schema: &MusicSchema) -> Result<Self, AppError> {
        let reader = index
            .reader_builder()
            // Searcher sees new documents within ~500ms of a writer commit.
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| AppError::Search {
                kind:   SearchErrorKind::OpenFailed,
                detail: e.to_string(),
            })?;

        Ok(Self { reader, schema: schema.clone() })
    }

    pub fn search(&self, raw_query: &str, limit: usize) -> Result<Vec<TrackSummary>, AppError> {
        let trimmed = raw_query.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }

        let tokens = tokenize_query(trimmed);
        if tokens.is_empty() {
            return Ok(vec![]);
        }

        let query  = self.build_query(&tokens);
        let reader = self.reader.searcher();

        let top_docs = reader
            .search(&*query, &TopDocs::with_limit(limit))
            .map_err(|e| AppError::Search {
                kind:   SearchErrorKind::ReadFailed,
                detail: e.to_string(),
            })?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (_score, addr) in top_docs {
            let doc: TantivyDocument = reader
                .doc(addr)
                .map_err(|e| AppError::Search {
                    kind:   SearchErrorKind::ReadFailed,
                    detail: e.to_string(),
                })?;
            if let Some(summary) = self.doc_to_summary(&doc) {
                results.push(summary);
            }
        }

        Ok(results)
    }

    /// Build the full compound query for a tokenized input.
    ///
    /// Strategy:
    ///   - Interior tokens (all except last):  FuzzyTermQuery(distance=1)
    ///   - Last token:                         FuzzyTermQuery::new_prefix(distance=1)
    ///     This matches "bohemi*" exactly (prefix) OR "bohemik" (1 edit).
    ///   - Each token produces a Should-BooleanQuery across all fields,
    ///     each field wrapped in a BoostQuery.
    ///   - All per-token sub-queries are combined with Must semantics:
    ///     every token must match somewhere in the document.
    ///
    /// Example — query "queen boh":
    ///   Must[
    ///     Should[
    ///       Boost(Fuzzy("queen", title,  d=1), 4.0),
    ///       Boost(Fuzzy("queen", artist, d=1), 3.0), ...
    ///     ],
    ///     Should[
    ///       Boost(FuzzyPrefix("boh", title,  d=1), 4.0),
    ///       Boost(FuzzyPrefix("boh", artist, d=1), 3.0), ...
    ///     ],
    ///   ]
    fn build_query(&self, tokens: &[String]) -> Box<dyn Query> {
        use tantivy::query::Occur;

        let n = tokens.len();

        let mut must_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(n);

        for (i, token) in tokens.iter().enumerate() {
            let is_last = i == n - 1;
            let sub = self.build_token_query(token, is_last);
            must_clauses.push((Occur::Must, sub));
        }

        match must_clauses.len() {
            0 => Box::new(AllQuery),
            1 => must_clauses.remove(0).1,
            _ => Box::new(BooleanQuery::new(must_clauses)),
        }
    }

    /// Build the per-token Should-query across all fields with boosts.
    fn build_token_query(&self, token: &str, is_last: bool) -> Box<dyn Query> {
        use tantivy::query::Occur;

        let fields: Vec<(tantivy::schema::Field, f32)> = vec![
            (self.schema.title,    4.0),
            (self.schema.artist,   3.0),
            (self.schema.album,    2.0),
            (self.schema.composer, 1.5),
            (self.schema.genre,    1.0),
            (self.schema.lyricist, 0.8),
        ];

        let mut should: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(fields.len());

        for (field, boost) in fields {
            let term = Term::from_field_text(field, token);

            // transposition_cost_one = true:
            //   "teh" → "the" counts as distance 1 (transposition),
            //   not distance 2 (delete + insert). More permissive.
            let fuzzy: Box<dyn Query> = if is_last {
                Box::new(FuzzyTermQuery::new_prefix(term, 1, true))
            } else {
                Box::new(FuzzyTermQuery::new(term, 1, true))
            };

            let boosted = Box::new(BoostQuery::new(fuzzy, boost));
            should.push((Occur::Should, boosted));
        }

        Box::new(BooleanQuery::new(should))
    }

    /// Extract a `TrackSummary` from a retrieved Tantivy document.
    /// Returns None only if `track_id` or `title` are missing — both
    /// are always set at index time so this should never return None
    /// on a healthy index.
    fn doc_to_summary(&self, doc: &TantivyDocument) -> Option<TrackSummary> {
        let s = &self.schema;

        let track_id = doc
            .get_first(s.track_id)?
            .as_str()?
            .parse::<uuid::Uuid>()
            .ok()?;

        let title = doc
            .get_first(s.title)?
            .as_str()?
            .to_owned();

        // For multi-value artist fields, collect all stored values and
        // join them as the display string. The first value is the primary
        // artist (insertion order preserved by Tantivy).
        let artist_display = doc
            .get_first(s.artist)
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let album_title = doc
            .get_first(s.album)
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        Some(TrackSummary {
            id: track_id,
            title,
            artist_display,
            album_title,
            ..TrackSummary::default()
        })
    }
}
```

`TrackSummary` must derive `Default`. Add `#[derive(Default)]` to it in
`crates/domain/src/`. Fields populated after a track is selected for
playback (`blob_location`, `duration_ms`, etc.) are resolved from
PostgreSQL by the `/play` handler — they are intentionally absent here.

---

#### `src/indexer.rs`

```rust
use sqlx::PgPool;
use tantivy::{IndexWriter, TantivyDocument};
use uuid::Uuid;
use std::collections::HashSet;

use application::{AppError, SearchErrorKind};
use crate::schema::MusicSchema;

// Flush to disk every N documents during a full rebuild.
// Keeps peak memory bounded without committing too frequently.
const COMMIT_BATCH_SIZE: usize = 500;

/// Row returned by the denormalization query.
/// All array columns are COALESCE'd to empty arrays — never null.
pub struct TrackRow {
    pub track_id:          Uuid,
    pub title:             String,
    pub artist_display:    Option<String>,
    pub year:              Option<i32>,
    pub bpm:               Option<i32>,
    pub track_genres:      Vec<String>,
    pub album_title:       Option<String>,
    pub album_genres:      Vec<String>,
    pub primary_artists:   Vec<String>,
    pub artist_sort_names: Vec<String>,
    pub composers:         Vec<String>,
    pub lyricists:         Vec<String>,
    pub featured_artists:  Vec<String>,
}

// The single SQL query used for both full rebuild (no WHERE on t.id)
// and single-track reindex (WHERE t.id = $1).
// Written as a macro-accessible const so sqlx::query_as! can validate
// it at compile time. The single-track variant appends AND t.id = $1.
pub const BASE_QUERY: &str = r#"
    SELECT
        t.id                                                     AS "track_id: Uuid",
        t.title,
        t.artist_display,
        t.year,
        t.bpm,
        COALESCE(t.genres, '{}'::text[])                         AS track_genres,
        al.title                                                 AS album_title,
        COALESCE(al.genres, '{}'::text[])                        AS album_genres,
        COALESCE(
            array_agg(ar.name)      FILTER (WHERE ta.role = 'primary'),
            '{}'::text[]
        )                                                        AS primary_artists,
        COALESCE(
            array_agg(ar.sort_name) FILTER (WHERE ta.role = 'primary'),
            '{}'::text[]
        )                                                        AS artist_sort_names,
        COALESCE(
            array_agg(ar.name)      FILTER (WHERE ta.role = 'composer'),
            '{}'::text[]
        )                                                        AS composers,
        COALESCE(
            array_agg(ar.name)      FILTER (WHERE ta.role = 'lyricist'),
            '{}'::text[]
        )                                                        AS lyricists,
        COALESCE(
            array_agg(ar.name)      FILTER (WHERE ta.role = 'featuring'),
            '{}'::text[]
        )                                                        AS featured_artists
    FROM tracks t
    LEFT JOIN albums        al ON al.id       = t.album_id
    LEFT JOIN track_artists ta ON ta.track_id = t.id
    LEFT JOIN artists       ar ON ar.id       = ta.artist_id
    WHERE t.enrichment_status = 'done'
    GROUP BY t.id, al.title, al.genres
    ORDER BY t.id
"#;

/// Build a Tantivy document from a denormalized database row.
pub fn build_document(row: &TrackRow, s: &MusicSchema) -> TantivyDocument {
    let mut doc = TantivyDocument::default();

    doc.add_text(s.track_id, &row.track_id.to_string());
    doc.add_text(s.title,    &row.title);

    // Artists — deduplicated, each value indexed as an independent token stream.
    // Insertion order: primary name, sort_name, featuring.
    let mut seen: HashSet<&str> = HashSet::new();
    for name in row.primary_artists.iter()
        .chain(row.artist_sort_names.iter())
        .chain(row.featured_artists.iter())
    {
        if seen.insert(name.as_str()) {
            doc.add_text(s.artist, name);
        }
    }
    // Fallback: artist_display for tracks without full artist rows yet.
    if row.primary_artists.is_empty() {
        if let Some(ref ad) = row.artist_display {
            if seen.insert(ad.as_str()) {
                doc.add_text(s.artist, ad);
            }
        }
    }

    if let Some(ref album) = row.album_title {
        doc.add_text(s.album, album);
    }

    // Genres: merge track + album, deduplicated
    let genres: HashSet<&str> = row.track_genres.iter()
        .chain(row.album_genres.iter())
        .map(String::as_str)
        .collect();
    for g in genres {
        doc.add_text(s.genre, g);
    }

    for name in &row.composers { doc.add_text(s.composer, name); }
    for name in &row.lyricists { doc.add_text(s.lyricist, name); }

    doc.add_u64(s.year, row.year.map(|y| y as u64).unwrap_or(0));
    doc.add_u64(s.bpm,  row.bpm.map(|b| b as u64).unwrap_or(0));

    doc
}

/// Full rebuild. Clears the index and reindexes all 'done' tracks.
/// Called at startup before any workers are spawned.
/// Returns the number of documents indexed.
pub async fn rebuild_index(
    writer: &mut IndexWriter,
    pool:   &PgPool,
    schema: &MusicSchema,
) -> Result<usize, AppError> {

    writer.delete_all_documents().map_err(write_err)?;

    let rows = sqlx::query_as!(TrackRow, BASE_QUERY)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Search {
            kind:   SearchErrorKind::RebuildFailed,
            detail: e.to_string(),
        })?;

    let total = rows.len();

    for (i, row) in rows.iter().enumerate() {
        writer.add_document(build_document(row, schema)).map_err(write_err)?;

        if (i + 1) % COMMIT_BATCH_SIZE == 0 {
            writer.commit().map_err(write_err)?;
        }
    }

    writer.commit().map_err(write_err)?;

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
    writer:   &mut IndexWriter,
    pool:     &PgPool,
    schema:   &MusicSchema,
    track_id: Uuid,
) -> Result<(), AppError> {

    // Delete any existing document with this track_id
    let id_term = tantivy::Term::from_field_text(
        schema.track_id,
        &track_id.to_string(),
    );
    writer.delete_term(id_term);

    let row = sqlx::query_as!(
        TrackRow,
        // BASE_QUERY with single-track filter
        r#"
        SELECT
            t.id                                                     AS "track_id: Uuid",
            t.title,
            t.artist_display,
            t.year,
            t.bpm,
            COALESCE(t.genres, '{}'::text[])                         AS track_genres,
            al.title                                                 AS album_title,
            COALESCE(al.genres, '{}'::text[])                        AS album_genres,
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'primary'),
                '{}'::text[]
            )                                                        AS primary_artists,
            COALESCE(
                array_agg(ar.sort_name) FILTER (WHERE ta.role = 'primary'),
                '{}'::text[]
            )                                                        AS artist_sort_names,
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'composer'),
                '{}'::text[]
            )                                                        AS composers,
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'lyricist'),
                '{}'::text[]
            )                                                        AS lyricists,
            COALESCE(
                array_agg(ar.name)      FILTER (WHERE ta.role = 'featuring'),
                '{}'::text[]
            )                                                        AS featured_artists
        FROM tracks t
        LEFT JOIN albums        al ON al.id       = t.album_id
        LEFT JOIN track_artists ta ON ta.track_id = t.id
        LEFT JOIN artists       ar ON ar.id       = ta.artist_id
        WHERE t.enrichment_status = 'done'
          AND t.id = $1
        GROUP BY t.id, al.title, al.genres
        "#,
        track_id
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Search {
        kind:   SearchErrorKind::ReadFailed,
        detail: e.to_string(),
    })?;

    if let Some(row) = row {
        writer
            .add_document(build_document(&row, schema))
            .map_err(write_err)?;
    }

    writer.commit().map_err(write_err)?;

    tracing::debug!(
        %track_id,
        operation = "search.track_reindexed",
        "track reindexed in Tantivy"
    );

    Ok(())
}

fn write_err(e: tantivy::TantivyError) -> AppError {
    AppError::Search {
        kind:   SearchErrorKind::WriteFailed,
        detail: e.to_string(),
    }
}
```

Note: `sqlx::query_as!` requires compile-time DB connectivity. Run
`cargo sqlx prepare --workspace` after the migration is applied. If the
`TrackRow` struct fields don't match the query columns exactly, the
compiler will tell you.

---

#### `src/lib.rs`

```rust
pub mod schema;
pub mod tokenizer;
pub mod indexer;
pub mod searcher;

use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tantivy::{Index, IndexWriter, directory::MmapDirectory};
use uuid::Uuid;
use async_trait::async_trait;

use application::{
    AppError, SearchErrorKind,
    ports::repository::TrackSearchPort,
};
use domain::TrackSummary;
use sqlx::PgPool;

use schema::MusicSchema;
use tokenizer::register_music_tokenizer;
use indexer::{rebuild_index, reindex_track};
use searcher::MusicSearcher;

// 50 MB write buffer. Trades memory for fewer intermediate commits.
// Minimum allowed by Tantivy is 15 MB.
const WRITER_HEAP_BYTES: usize = 50_000_000;

pub struct TantivySearchAdapter {
    searcher: MusicSearcher,
    writer:   Arc<Mutex<IndexWriter>>,
    schema:   MusicSchema,
    pool:     PgPool,
}

impl TantivySearchAdapter {
    /// Open an existing index or create a new empty one.
    /// Synchronous — safe to call before the Tokio runtime starts.
    /// Call `rebuild_all().await` immediately after if the index is new
    /// or if a full reindex is required (startup, /rescan).
    pub fn open_or_create(path: PathBuf, pool: PgPool) -> Result<Self, AppError> {
        let schema = MusicSchema::build();

        let open_err = |e: tantivy::TantivyError| AppError::Search {
            kind:   SearchErrorKind::OpenFailed,
            detail: e.to_string(),
        };
        let io_err = |e: std::io::Error| AppError::Search {
            kind:   SearchErrorKind::OpenFailed,
            detail: format!("index dir I/O: {e}"),
        };

        let index = if path.join("meta.json").exists() {
            // meta.json present → existing index
            let dir = MmapDirectory::open(&path).map_err(|e| AppError::Search {
                kind:   SearchErrorKind::OpenFailed,
                detail: e.to_string(),
            })?;
            Index::open(dir).map_err(open_err)?
        } else {
            std::fs::create_dir_all(&path).map_err(io_err)?;
            let dir = MmapDirectory::open(&path).map_err(|e| AppError::Search {
                kind:   SearchErrorKind::OpenFailed,
                detail: e.to_string(),
            })?;
            Index::create(dir, schema.schema.clone(), Default::default())
                .map_err(open_err)?
        };

        // Must be called on every open — Tantivy does not persist tokenizer
        // registrations to disk.
        register_music_tokenizer(&index);

        let writer = index.writer(WRITER_HEAP_BYTES).map_err(open_err)?;
        let searcher = MusicSearcher::new(&index, &schema)?;

        Ok(Self {
            searcher,
            writer: Arc::new(Mutex::new(writer)),
            schema,
            pool,
        })
    }

    /// Full rebuild from PostgreSQL. Blocks until complete.
    /// Safe to call concurrently with searches — reader and writer are
    /// independent in Tantivy's MVCC model.
    pub async fn rebuild_all(&self) -> Result<usize, AppError> {
        let mut w = self.writer.lock().await;
        rebuild_index(&mut w, &self.pool, &self.schema).await
    }

    /// Reindex a single track after enrichment completes.
    pub async fn reindex_one(&self, track_id: Uuid) -> Result<(), AppError> {
        let mut w = self.writer.lock().await;
        reindex_track(&mut w, &self.pool, &self.schema, track_id).await
    }
}

#[async_trait]
impl TrackSearchPort for TantivySearchAdapter {
    async fn autocomplete(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TrackSummary>, AppError> {
        // Tantivy searching is CPU-bound and synchronous.
        // spawn_blocking prevents it from stalling the async executor
        // under concurrent autocomplete requests.
        let searcher = self.searcher.clone();
        let query    = query.to_owned();

        tokio::task::spawn_blocking(move || searcher.search(&query, limit))
            .await
            .map_err(|e| AppError::Search {
                kind:   SearchErrorKind::ReadFailed,
                detail: format!("search task join error: {e}"),
            })?
    }

    async fn rebuild_index(&self) -> Result<usize, AppError> {
        self.rebuild_all().await
    }

    async fn reindex_track(&self, track_id: Uuid) -> Result<(), AppError> {
        self.reindex_one(track_id).await
    }
}
```

---

### Step 4 — `TrackSearchPort` Additions

In `crates/application/src/ports/repository.rs`, update the trait:

```rust
#[async_trait]
pub trait TrackSearchPort: Send + Sync {
    async fn autocomplete(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TrackSummary>, AppError>;

    /// Full index rebuild from PostgreSQL source of truth.
    async fn rebuild_index(&self) -> Result<usize, AppError>;

    /// Reindex a single track by UUID after enrichment completes.
    async fn reindex_track(&self, track_id: Uuid) -> Result<(), AppError>;
}
```

---

### Step 5 — Config

In `crates/shared-config/src/lib.rs`, add:

```rust
/// Absolute path to the Tantivy index directory.
///
/// MUST be on local disk. Do NOT point this at the NAS SMB mount.
/// Memory-mapped files over a network filesystem cause undefined behavior
/// on network interruption. The index is fully reconstructable from
/// PostgreSQL in ~2 seconds.
///
/// Env var: TANTIVY_INDEX_PATH
/// Default: "./search_index"
pub tantivy_index_path: std::path::PathBuf,
```

Add to `.env.example`:
```
# Tantivy search index directory — LOCAL disk only, not NAS.
# Fully reconstructable from PostgreSQL; safe to delete.
TANTIVY_INDEX_PATH=./search_index
```

---

### Step 6 — Remove Old Search Code

In `crates/adapters-persistence/src/repositories/track_repository.rs`:
- Remove `impl TrackSearchPort for TrackRepositoryImpl`
- Remove the `autocomplete` method and its SQL query
- Remove any import of `TrackSearchPort` from this file

If `track_repository.rs` has a `use application::ports::repository::TrackSearchPort`
import that becomes unused, remove it.

---

### Step 7 — Post-Enrichment Hook

In `crates/application/src/tag_writer_worker.rs`, after the final
`enrichment_status = 'done'` update:

```rust
// Non-fatal: a failed reindex is caught on the next startup rebuild.
// Do not fail the enrichment pipeline or mark the track as failed.
if let Err(e) = self.search_port.reindex_track(track.id).await {
    tracing::warn!(
        track_id   = %track.id,
        error      = %e,
        error.kind = %e.kind_str(),
        operation  = "search.reindex_track_failed",
        "Tantivy reindex failed after enrichment — will reconcile at next startup"
    );
}
```

Add `search_port: Arc<dyn TrackSearchPort>` to `TagWriterWorker`'s struct
and constructor. Update `main.rs` to inject it.

---

### Step 8 — Wiring in `apps/bot/src/main.rs`

```rust
// -- after pool is constructed --

// 1. NAS path guard
if config.tantivy_index_path.starts_with(&config.media_root) {
    tracing::warn!(
        operation = "search.startup_check",
        path = %config.tantivy_index_path.display(),
        "TANTIVY_INDEX_PATH is under MEDIA_ROOT — index must be on local disk, not NAS"
    );
}

// 2. Open or create the index (synchronous)
let search = Arc::new(
    TantivySearchAdapter::open_or_create(
        config.tantivy_index_path.clone(),
        pool.clone(),
    )
    .expect("failed to open Tantivy search index"),
);

// 3. Full rebuild before workers or Discord client start
let doc_count = search
    .rebuild_all()
    .await
    .expect("failed to build Tantivy index from PostgreSQL");

tracing::info!(
    documents = doc_count,
    operation = "search.startup_rebuild_complete",
    "Tantivy search index ready"
);

// 4. Pass Arc<dyn TrackSearchPort> to /play autocomplete
// 5. Pass Arc<dyn TrackSearchPort> to TagWriterWorker
// 6. Pass Arc<dyn TrackSearchPort> to /rescan handler (calls rebuild_index)
```

The `/rescan` command should call `search_port.rebuild_index().await`
after the filesystem scan completes, so the index reflects any newly
enriched tracks.

---

### Verification

```bash
# 1. Clean DB + fresh migrations
cargo sqlx database drop && cargo sqlx database create
cargo sqlx migrate run

# 2. Query cache fresh
cargo sqlx prepare --workspace

# 3. Build — zero warnings, zero errors
cargo build --workspace 2>&1

# 4. Tests
cargo test --workspace 2>&1

# 5. Confirm search_vector / search_text columns gone
psql $DATABASE_URL -c "\d tracks" | grep -E "search_vector|search_text"
# Expected: no output

# 6. Confirm pg_trgm gone
psql $DATABASE_URL -c "SELECT * FROM pg_extension WHERE extname = 'pg_trgm';"
# Expected: 0 rows

# 7. Manual smoke tests in Discord
# /play bohemian               → "Bohemian Rhapsody" in results
# /play radiohed               → fuzzy → Radiohead tracks (distance=1)
# /play ok computer            → Radiohead album tracks (album field)
# /play hip hop                → genre-matched tracks
# /play beethoven              → composer field matches
# /play sigur ros              → AsciiFoldingFilter: "Sigur Rós" found
# /play queen boh              → cross-field: artist "Queen" + title prefix "boh*"
```

---

### API Reference Notes for the Agent

Before coding, open `https://docs.rs/tantivy/0.26.0` and confirm:

| Symbol | Expected location in 0.26 |
|--------|--------------------------|
| `FuzzyTermQuery` | `tantivy::query::FuzzyTermQuery` |
| `FuzzyTermQuery::new_prefix` | same struct, check signature |
| `BooleanQuery`, `BoostQuery` | `tantivy::query::*` |
| `TantivyDocument` | `tantivy::TantivyDocument` (renamed from `Document` in 0.22) |
| `SimpleTokenizer` | `tantivy::tokenizer::SimpleTokenizer` |
| `AsciiFoldingFilter` | `tantivy::tokenizer::AsciiFoldingFilter` |
| `LowerCaser` | `tantivy::tokenizer::LowerCaser` |
| `TextAnalyzer::builder` | `tantivy::tokenizer::TextAnalyzer` |
| `ReloadPolicy::OnCommitWithDelay` | `tantivy::ReloadPolicy` (renamed in 0.22) |
| `MmapDirectory` | `tantivy::directory::MmapDirectory` |
| `IndexRecordOption` | `tantivy::schema::IndexRecordOption` |

If any symbol has moved or been renamed between 0.22 and 0.26, use the
0.26 path. Do not guess — check the docs before using.

---

### Constraints

- Modify existing migrations in-place. No new migration file.
- Do not add any stemmer. See schema.rs comments.
- `TANTIVY_INDEX_PATH` must not point to the NAS. The startup guard is
  mandatory, not optional.
- All Tantivy errors surface as `AppError::Search`. No `unwrap()` on
  index operations after startup initialization.
- Post-enrichment reindex failures are non-fatal warnings, not errors.
  The enrichment pipeline must never be blocked by a search index failure.
- `cargo sqlx prepare --workspace` must pass with zero errors before
  any other step is considered complete.
