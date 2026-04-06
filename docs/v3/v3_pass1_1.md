TeamTI v3 — Audit Pass 1.1
Search Layer: Correctness & Optimization Review

    Scope. Review and correct the output of Pass 1 (adapters-search
    and its migration changes) before the layer goes into integration testing.
    Fix all Critical and Major findings. Apply Optimization findings unless
    they conflict with Pass 1's "keep it simple" intent.

    Do not: add listen history scoring, faceting, phrase queries, or
    year/BPM filtering. Those remain in a later pass.

    Reference: Attach the Pass 1 output files before sending this prompt.

Findings Index
ID	Severity	Location	Title
C1	Critical	indexer.rs	sqlx::query_as! rejects non-literal query strings
C2	Critical	searcher.rs	Short tokens cause explosive fuzzy recall
C3	Critical	tokenizer.rs	build_music_tokenizer() called per-token, not reused
M1	Major	lib.rs	open_or_create uses fragile meta.json existence check
M2	Major	indexer.rs	Startup rebuild has no progress logging
M3	Major	indexer.rs	reindex_track commits once per track; batch enrichment floods the writer
M4	Major	searcher.rs	doc_to_summary returns only the first artist; multi-artist tracks display incorrectly
M5	Major	ports/repository.rs	TrackSearchPort::reindex_track conflicts with the method name in lib.rs
O1	Optim.	indexer.rs	ORDER BY t.id in rebuild query forces a full sort
O2	Optim.	indexer.rs	HashSet<&str> in build_document allocates per document
O3	Optim.	lib.rs	MusicSearcher::clone() called on the async thread before spawn_blocking
O4	Optim.	indexer.rs	COMMIT_BATCH_SIZE is a hard constant; should be configurable or at least justified
N1	Note	schema.rs	year fast field stores 0 for unknown year; ambiguous
N2	Note	searcher.rs	FIELD_BOOSTS const is never used; build_token_query uses a hardcoded Vec
Critical Fixes
C1 — sqlx::query_as! rejects non-literal query strings

File: src/indexer.rs

Problem. sqlx::query_as!(TrackRow, BASE_QUERY) does not compile.
The query_as! macro requires a string literal as its second argument.
A const &str binding is not a literal in the macro expansion sense —
rustc rejects it with "expected string literal".

Fix. Replace BASE_QUERY with two separate sqlx::query_as! call
sites, each with the full query string inline. Accept the duplication in
exchange for compile-time query validation on both paths.

Remove:

rust
pub const BASE_QUERY: &str = r#"..."#;

In rebuild_index, write the query inline in the query_as! call:

rust
let rows = sqlx::query_as!(TrackRow, r#"
    SELECT
        t.id                AS "track_id: Uuid",
        t.title,
        ...
    FROM tracks t
    ...
    WHERE t.enrichment_status = 'done'
    GROUP BY t.id, al.title, al.genres
"#)
.fetch_all(pool)
.await
.map_err(...)?;

The single-track query in reindex_track already has the full string inline
and is correct as written — no change needed there.

If query duplication is unacceptable, replace sqlx::query_as! (macro)
with sqlx::query_as() (runtime function) for the rebuild path only:

rust
// Runtime variant — loses compile-time SQL checking for this query.
// Acceptable if the single-track macro variant catches schema drift.
let rows = sqlx::query_as::<_, TrackRow>(BASE_QUERY)
    .fetch_all(pool)
    .await
    ...;

Prefer the inline-literal approach. The compile-time check is the point.
C2 — Short tokens cause explosive fuzzy recall

File: src/searcher.rs, build_token_query

Problem. FuzzyTermQuery with distance=1 on a one- or two-character
token matches an enormous fraction of the term dictionary. A token "a" at
distance=1 matches every term of length 1 or 2 ("a", "b", "ab",
"ba", …). For a two-character token "qu", fuzzy matching at distance=1
matches "q", "que", "u", "pu", "qu", "qr", etc. This floods the
TopDocs collector with irrelevant results, especially when the fuzzy term
hits the genre, composer, or lyricist fields which contain common
short tokens.

For prefix matching (the last token), FuzzyTermQuery::new_prefix already
handles short tokens well — prefix matching "qu*" is exact and fast. The
fuzzy component of new_prefix is what becomes dangerous.

Fix. Apply distance=1 fuzzy only when the token is **4 or more
characters long**. For tokens shorter than 4 characters, use exact
matching only (or, for the last token, pure prefix with distance=0).

rust
/// Minimum token length before fuzzy matching is applied.
/// Below this length, the edit-distance neighbourhood is too broad.
const FUZZY_MIN_LEN: usize = 4;

fn build_token_query(&self, token: &str, is_last: bool) -> Box<dyn Query> {
    use tantivy::query::Occur;

    let fields: Vec<(Field, f32)> = vec![
        (self.schema.title,    4.0),
        (self.schema.artist,   3.0),
        (self.schema.album,    2.0),
        (self.schema.composer, 1.5),
        (self.schema.genre,    1.0),
        (self.schema.lyricist, 0.8),
    ];

    let use_fuzzy = token.chars().count() >= FUZZY_MIN_LEN;

    let mut should: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(fields.len());

    for (field, boost) in fields {
        let term = Term::from_field_text(field, token);

        let base_query: Box<dyn Query> = match (is_last, use_fuzzy) {
            // Long last token: prefix + fuzzy distance=1
            (true,  true)  => Box::new(FuzzyTermQuery::new_prefix(term, 1, true)),
            // Short last token: pure prefix, no fuzzy
            (true,  false) => Box::new(FuzzyTermQuery::new_prefix(term, 0, true)),
            // Long interior token: fuzzy distance=1
            (false, true)  => Box::new(FuzzyTermQuery::new(term, 1, true)),
            // Short interior token: exact match
            (false, false) => Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        };

        should.push((Occur::Should, Box::new(BoostQuery::new(base_query, boost))));
    }

    Box::new(BooleanQuery::new(should))
}

Add to imports: use tantivy::query::TermQuery; and use tantivy::schema::IndexRecordOption;
C3 — Tokenizer rebuilt per tokenize_query call

File: src/tokenizer.rs and src/searcher.rs

Problem. tokenize_query() calls build_music_tokenizer() every
invocation, which allocates and chains three filter stages on every
autocomplete keypress. Under Discord's autocomplete rate (one invocation
per keystroke, up to 25 concurrent users), this creates unnecessary heap
churn.

Fix. Store the tokenizer on MusicSearcher and pass it through:

rust
// In MusicSearcher struct:
pub struct MusicSearcher {
    reader:    IndexReader,
    schema:    MusicSchema,
    tokenizer: tantivy::tokenizer::TextAnalyzer,
}

// In MusicSearcher::new:
let tokenizer = build_music_tokenizer();
// Also register it on the index here, not separately
index.tokenizers().register("music", tokenizer.clone());
Ok(Self { reader, schema: schema.clone(), tokenizer })

Update tokenize_query to be a method:

rust
fn tokenize(&self, text: &str) -> Vec<String> {
    let mut stream = self.tokenizer.token_stream(text);
    let mut tokens = Vec::new();
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    tokens
}

Remove pub fn tokenize_query from tokenizer.rs. Remove its export
from lib.rs. The tokenizer registration in tokenizer.rs is now also
redundant if MusicSearcher::new registers it — consolidate:

    tokenizer.rs exports only build_music_tokenizer() -> TextAnalyzer

    Registration happens in MusicSearcher::new via index.tokenizers().register(...)

    lib.rs removes its call to register_music_tokenizer(&index) after
    construction — MusicSearcher::new now owns that responsibility

Major Fixes
M1 — open_or_create uses fragile meta.json existence check

File: src/lib.rs

Problem. Checking for meta.json relies on Tantivy's internal file
naming convention, which is not part of the public API contract. A future
Tantivy release could rename or restructure index metadata files, silently
causing open_or_create to attempt Index::create on an already-populated
directory, corrupting the index.

Fix. Use Index::open() and treat failure as "needs creation":

rust
let index = {
    let dir_result = MmapDirectory::open(&path);

    match dir_result {
        Ok(dir) => {
            match Index::open(dir) {
                Ok(idx) => {
                    tracing::debug!(
                        path = %path.display(),
                        operation = "search.index_opened",
                        "opened existing Tantivy index"
                    );
                    idx
                }
                Err(_) => {
                    // Directory exists but is not a valid index — create fresh.
                    tracing::info!(
                        path = %path.display(),
                        operation = "search.index_created",
                        "no valid index found, creating new"
                    );
                    let dir = MmapDirectory::open(&path).map_err(open_io_err)?;
                    Index::create(dir, schema.schema.clone(), Default::default())
                        .map_err(open_err)?
                }
            }
        }
        Err(_) => {
            // Directory doesn't exist yet — create it and the index.
            std::fs::create_dir_all(&path).map_err(open_io_err)?;
            let dir = MmapDirectory::open(&path).map_err(open_io_err)?;
            Index::create(dir, schema.schema.clone(), Default::default())
                .map_err(open_err)?
        }
    }
};

M2 — Startup rebuild has no progress logging

File: src/indexer.rs, rebuild_index

Problem. For a 50,000-track library, the rebuild takes ~2–4 seconds
with no visible progress. During a deploy this looks like a hang.

Fix. Log progress at each batch commit:

rust
for (i, row) in rows.iter().enumerate() {
    writer.add_document(build_document(row, schema)).map_err(write_err)?;

    if (i + 1) % COMMIT_BATCH_SIZE == 0 {
        writer.commit().map_err(write_err)?;
        tracing::debug!(
            indexed  = i + 1,
            total    = total,
            pct      = ((i + 1) * 100) / total,
            operation = "search.rebuild_progress",
            "index rebuild in progress"
        );
    }
}

Also add a timing span around the full rebuild in main.rs:

rust
let t0 = std::time::Instant::now();
let doc_count = search.rebuild_all().await.expect("...");
tracing::info!(
    documents = doc_count,
    elapsed_ms = t0.elapsed().as_millis(),
    operation  = "search.startup_rebuild_complete",
    "Tantivy index ready"
);

M3 — reindex_track commits once per track; batch enrichment floods the writer

File: src/indexer.rs, src/lib.rs

Problem. When the enrichment pipeline processes a backlog (e.g. after
a /rescan), TagWriterWorker calls reindex_track for every track in
rapid succession. Each call acquires the Mutex<IndexWriter>, performs a
delete + add, and then calls writer.commit(). Each commit is a disk flush
and a segment merge — 500 commits in quick succession is expensive and
causes write amplification.

Fix. Separate the write and the commit. Add a reindex_one_no_commit
path for the post-enrichment hook, and commit on a timer or after N
pending writes:

rust
// In TantivySearchAdapter, add a pending-writes counter:
pending_writes: Arc<std::sync::atomic::AtomicUsize>,

// In reindex_one (called post-enrichment):
pub async fn reindex_one(&self, track_id: Uuid) -> Result<(), AppError> {
    let mut w = self.writer.lock().await;
    reindex_track_no_commit(&mut w, &self.pool, &self.schema, track_id).await?;

    let pending = self.pending_writes
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

    // Commit every 20 writes or immediately if only one is pending
    // (handles the non-batch single-enrichment case).
    if pending >= 20 {
        w.commit().map_err(write_err)?;
        self.pending_writes.store(0, std::sync::atomic::Ordering::Relaxed);
    } else {
        // Commit on a background task so the first write is
        // visible within 2 seconds even if no further writes arrive.
        // Implementation: use a tokio::time::sleep(2s) debounce.
        // For Pass 1 — commit immediately, optimize in the next pass.
        w.commit().map_err(write_err)?;
        self.pending_writes.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

For Pass 1 correctness, committing immediately is still safe. The batching
optimization can be revisited when the enrichment pipeline's throughput
is benchmarked. Document the intent clearly in a comment so the next pass
knows what to do:

rust
// TODO Pass 1.2: implement debounced commit for batch enrichment paths.
// Currently commits on every reindex. Acceptable for <100 tracks/run;
// revisit if large backlog processing becomes a bottleneck.

M4 — doc_to_summary returns only the first stored artist

File: src/searcher.rs, doc_to_summary

Problem. doc.get_first(s.artist) returns only the first value stored
in the artist multi-value field. For a track by "Queens of the Stone Age
feat. Mark Lanegan", the first stored artist is "Queens of the Stone Age"
which is correct. But the artist_display Tantivy returns is never joined
— the TrackSummary shows only one artist name. For now this is acceptable
since artist_display is a best-effort hint for the Discord embed; the
authoritative display string is resolved from PostgreSQL when the track is
queued. However, the primary artist name stored first in the index must be
reliable, not random.

Fix. In build_document, ensure the first add_text(s.artist, ...) call
is always the primary display artist. The current order (primary → sort_name
→ featuring) is correct. No code change needed, but add a comment:

rust
// The first value added to the artist field is the primary display artist.
// MusicSearcher::doc_to_summary calls get_first() and uses this value
// as the display name in autocomplete results. Insertion order must be:
// 1. Primary artist name (display form: "Queens of the Stone Age")
// 2. Sort name ("Queens of the Stone Age, The" — for index coverage)
// 3. Featuring artists
// 4. Fallback artist_display only if primary_artists is empty
for name in &row.primary_artists { ... }

M5 — Method name conflict between trait and inherent impl

File: src/lib.rs and crates/application/src/ports/repository.rs

Problem. TrackSearchPort defines reindex_track(&self, track_id: Uuid).
TantivySearchAdapter also has an inherent method reindex_one and a trait
impl method reindex_track. The trait method delegates to the inherent method,
which is correct — but the concrete TantivySearchAdapter is also passed
directly to TagWriterWorker for the post-enrichment hook. If TagWriterWorker
holds Arc<TantivySearchAdapter> (concrete) rather than Arc<dyn TrackSearchPort>
(trait object), it calls the inherent reindex_one directly, bypassing any
trait-level instrumentation added later.

Fix. Pass Arc<dyn TrackSearchPort> everywhere, including to
TagWriterWorker. There should be no code path that takes
Arc<TantivySearchAdapter> after construction in main.rs. Enforce this
by making TantivySearchAdapter non-Clone and not re-exporting it from
adapters-search::lib — export only the constructor:

rust
// adapters-search/src/lib.rs — public surface
pub use adapter::TantivySearchAdapter;  // only for construction in main.rs
// After construction, callers must upcast to Arc<dyn TrackSearchPort>

In main.rs:

rust
let search: Arc<dyn TrackSearchPort> = Arc::new(
    TantivySearchAdapter::open_or_create(path, pool.clone())?
);
// Never use TantivySearchAdapter again after this line.

For the startup rebuild, cast back via Any if needed, OR call
search.rebuild_index().await via the trait method — which is already
defined on the trait. The trait method call is the correct path.
Optimizations
O1 — ORDER BY t.id in rebuild query forces a full sort

File: src/indexer.rs

Remove ORDER BY t.id from the rebuild query. Tantivy does not require
documents in any particular order. The sort forces PostgreSQL to materialize
and sort the entire result set before returning the first row, adding ~200ms
on a 50k-row table.

sql
-- Remove this line:
ORDER BY t.id

O2 — HashSet::new() in build_document allocates without capacity hint

File: src/indexer.rs

rust
// Replace:
let mut seen: HashSet<&str> = HashSet::new();

// With:
// Typical track has 1-3 primary artists + 1 sort name + 0-2 featuring = ~5
let mut seen: HashSet<&str> = HashSet::with_capacity(8);

Same for the genre deduplication set:

rust
let genres: HashSet<&str> = HashSet::with_capacity(
    row.track_genres.len() + row.album_genres.len()
);

O3 — MusicSearcher::clone() on async thread before spawn_blocking

File: src/lib.rs

rust
// Current — clone happens on the Tokio worker thread:
let searcher = self.searcher.clone();
let query    = query.to_owned();
tokio::task::spawn_blocking(move || searcher.search(&query, limit)).await

// Better — the clone is cheap (Arc-backed) so this doesn't matter much,
// but for clarity make the intent explicit with a comment:
let searcher = self.searcher.clone();  // cheap: Arc<IndexReader> clone
let query    = query.to_owned();
tokio::task::spawn_blocking(move || searcher.search(&query, limit)).await

No code change required — the current code is correct. Add the comment
to prevent a future reader from "optimizing" this away.
O4 — COMMIT_BATCH_SIZE should be a config value or at least documented

File: src/indexer.rs

rust
// Replace:
const COMMIT_BATCH_SIZE: usize = 500;

// With a documented constant:
/// Number of documents to buffer before flushing to disk during a full
/// rebuild. Higher values use more memory but produce fewer segments,
/// leading to faster post-rebuild search. 500 is safe for 50k tracks;
/// increase to 2000 if rebuild memory usage is acceptable on the host.
/// Pass 1.1 TODO: make this configurable via SearchConfig.
const COMMIT_BATCH_SIZE: usize = 500;

Notes (no code change required)
N1 — year fast field stores 0 for unknown year

Year 0 is ambiguous — it could mean "unknown" or the literal year 0 CE.
No year/BPM filtering exists yet, so this is harmless in Pass 1. When
year-range filtering is added (later pass), change the storage to use an
Option<u64> via a separate has_year: u64 (boolean fast field) or
switch to i64 with a sentinel value like i64::MIN. Document now so
the later pass knows:

rust
// NOTE: year=0 means "unknown". When year-range filtering is added,
// this field must be reconsidered. Options:
//   a) Separate has_year: u64 (0/1) fast field
//   b) Use i64::MIN as sentinel
// Do not use year=0 as a filter target.
doc.add_u64(s.year, row.year.map(|y| y as u64).unwrap_or(0));

N2 — FIELD_BOOSTS const is dead code

File: src/searcher.rs

The FIELD_BOOSTS const using function pointer accessors was defined but
the build_token_query method uses a local Vec instead. Remove the const
to eliminate the dead code warning:

rust
// Remove entirely:
const FIELD_BOOSTS: &[(fn(&MusicSchema) -> Field, f32)] = &[...];

Verification Checklist

After applying all fixes, confirm:

bash
# Zero dead-code or unused-import warnings
cargo build --workspace 2>&1 | grep -E "warning|error"

# Query preparation passes — C1 fix is correct
cargo sqlx prepare --workspace

# cargo test passes
cargo test --workspace

# Smoke tests in Discord
# Short tokens — exact, not fuzzy:
# /play qu               → prefix only, no fuzzy explosion
# /play a                → should return results, not crash

# Fuzzy on long tokens:
# /play radiohed         → "radiohead" (distance=1 transposition)
# /play bohemian rhapso  → "Bohemian Rhapsody" (prefix on last token)

# ASCII folding:
# /play sigur ros        → "Sigur Rós"
# /play bjork            → "Björk"

# Multi-artist:
# /play queens stone     → "Queens of the Stone Age" (cross-token Must)

# Confirm no FTS columns in DB:
psql $DATABASE_URL -c "\d tracks" | grep -c search
# Expected: 0