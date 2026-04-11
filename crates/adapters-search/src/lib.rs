pub mod indexer;
pub mod schema;
pub mod searcher;
pub mod tokenizer;

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use sqlx::PgPool;
use tantivy::{Index, IndexWriter, directory::MmapDirectory};
use tokio::sync::Mutex;
use uuid::Uuid;

use application::{AppError, SearchErrorKind, ports::search::TrackSearchPort};
use domain::track::TrackSummary;

use indexer::{rebuild_index, reindex_track};
use schema::MusicSchema;
use searcher::MusicSearcher;

// 50 MB write buffer. Trades memory for fewer intermediate commits.
// Minimum allowed by Tantivy is 15 MB.
const WRITER_HEAP_BYTES: usize = 50_000_000;

pub struct TantivySearchAdapter {
    searcher: MusicSearcher,
    writer: Arc<Mutex<IndexWriter>>,
    pending_writes: Arc<std::sync::atomic::AtomicUsize>,
    schema: MusicSchema,
    pool: PgPool,
}

impl TantivySearchAdapter {
    /// Open an existing index or create a new empty one.
    /// Call `rebuild_all().await` immediately after if the index is new
    /// or if a full reindex is required (startup, /rescan).
    pub fn open_or_create(path: &PathBuf, pool: PgPool) -> Result<Self, AppError> {
        let schema = MusicSchema::build();

        let open_err = |e: tantivy::TantivyError| AppError::Search {
            kind: SearchErrorKind::OpenFailed,
            detail: e.to_string(),
        };
        let io_err = |e: std::io::Error| AppError::Search {
            kind: SearchErrorKind::OpenFailed,
            detail: format!("index dir I/O: {e}"),
        };

        let index = {
            let dir_result = MmapDirectory::open(path);

            if let Ok(dir) = dir_result {
                if let Ok(idx) = Index::open(dir) {
                    tracing::debug!(
                        path = %path.display(),
                        operation = "search.index_opened",
                        "opened existing Tantivy index"
                    );
                    idx
                } else {
                    // Directory exists but is not a valid index — create fresh.
                    tracing::info!(
                        path = %path.display(),
                        operation = "search.index_created",
                        "no valid index found, creating new"
                    );
                    let dir = MmapDirectory::open(path).map_err(|e| AppError::Search {
                        kind: SearchErrorKind::OpenFailed,
                        detail: e.to_string(),
                    })?;
                    Index::create(
                        dir,
                        schema.schema.clone(),
                        tantivy::IndexSettings::default(),
                    )
                    .map_err(open_err)?
                }
            } else {
                // Directory doesn't exist yet — create it and the index.
                std::fs::create_dir_all(path).map_err(io_err)?;
                let dir = MmapDirectory::open(path).map_err(|e| AppError::Search {
                    kind: SearchErrorKind::OpenFailed,
                    detail: e.to_string(),
                })?;
                Index::create(
                    dir,
                    schema.schema.clone(),
                    tantivy::IndexSettings::default(),
                )
                .map_err(open_err)?
            }
        };

        let writer = index.writer(WRITER_HEAP_BYTES).map_err(open_err)?;
        let searcher = MusicSearcher::new(&index, &schema)?;

        Ok(Self {
            searcher,
            writer: Arc::new(Mutex::new(writer)),
            pending_writes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            schema,
            pool,
        })
    }

    /// Full rebuild from PostgreSQL. Blocks until complete.
    pub async fn rebuild_all(&self) -> Result<usize, AppError> {
        let mut w = self.writer.lock().await;
        rebuild_index(&mut w, &self.pool, &self.schema).await
    }

    /// Reindex a single track after enrichment completes.
    pub async fn reindex_one(&self, track_id: Uuid) -> Result<(), AppError> {
        let mut w = self.writer.lock().await;

        reindex_track(&mut w, &self.pool, &self.schema, track_id).await?;

        let pending = self
            .pending_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;

        // Commit every 20 writes to aggregate I/O or immediately if only one is pending
        // TODO Pass 1.2: implement debounced commit for batch enrichment paths.
        // Currently commits on every reindex. Acceptable for <100 tracks/run;
        // revisit if large backlog processing becomes a bottleneck.
        if pending >= 20 {
            w.commit().map_err(|e| AppError::Search {
                kind: SearchErrorKind::WriteFailed,
                detail: e.to_string(),
            })?;
            self.pending_writes
                .store(0, std::sync::atomic::Ordering::Relaxed);
        } else {
            w.commit().map_err(|e| AppError::Search {
                kind: SearchErrorKind::WriteFailed,
                detail: e.to_string(),
            })?;
            self.pending_writes
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }
}

#[async_trait]
impl TrackSearchPort for TantivySearchAdapter {
    async fn autocomplete(&self, query: &str, limit: usize) -> Result<Vec<TrackSummary>, AppError> {
        // Tantivy searching is CPU-bound and synchronous.
        // spawn_blocking prevents it from stalling the async executor
        // under concurrent autocomplete requests.
        let searcher = self.searcher.clone(); // cheap: Arc<IndexReader> clone
        let query = query.to_owned();

        tokio::task::spawn_blocking(move || searcher.search(&query, limit))
            .await
            .map_err(|e| AppError::Search {
                kind: SearchErrorKind::ReadFailed,
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
