pub mod indexer;
pub mod schema;
pub mod searcher;
pub mod tokenizer;

#[cfg(test)]
mod tests;

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use sqlx::PgPool;
use tantivy::{Index, IndexWriter, directory::MmapDirectory};
use tokio::sync::Mutex;
use uuid::Uuid;

use application::{AppError, SearchErrorKind, ports::search::MusicSearchPort};
use domain::search::{SearchFilter, SearchResult};
use indexer::ToSearchDoc;

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
                    // Schema migration guard: detect field count mismatch between
                    // the on-disk index and the current compiled schema. If they
                    // diverge, the old segments contain documents with a different
                    // field layout, which causes panics on FAST field access.
                    let on_disk_fields = idx.schema().fields().count();
                    let expected_fields = schema.schema.fields().count();

                    if on_disk_fields == expected_fields {
                        tracing::debug!(
                            path = %path.display(),
                            operation = "search.index_opened",
                            "opened existing Tantivy index"
                        );
                        idx
                    } else {
                        tracing::warn!(
                            on_disk_fields,
                            expected_fields,
                            path = %path.display(),
                            operation = "search.schema_migration",
                            "schema mismatch detected — deleting stale index and recreating"
                        );
                        // Drop the old index handle before removing files
                        drop(idx);
                        // Remove stale index directory and recreate
                        std::fs::remove_dir_all(path).map_err(io_err)?;
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
        let (rows, cache_rows) = indexer::fetch_rebuild_data(&self.pool).await?;
        let mut w = self.writer.lock().await;
        indexer::execute_rebuild(&mut w, &self.schema, &rows, &cache_rows)
    }

    /// Reindex a single track after enrichment completes.
    pub async fn reindex_one(&self, track_id: Uuid) -> Result<(), AppError> {
        let row_opt = indexer::fetch_single_track(&self.pool, track_id).await?;
        let mut w = self.writer.lock().await;

        indexer::execute_reindex_track(&mut w, &self.schema, track_id, row_opt.as_ref())?;

        let pending = self
            .pending_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;

        // Commit every 20 writes to aggregate I/O.
        if pending >= 20 {
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
impl MusicSearchPort for TantivySearchAdapter {
    async fn autocomplete(
        &self,
        query: &str,
        filter: SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>, AppError> {
        // Tantivy searching is CPU-bound and synchronous.
        // spawn_blocking prevents it from stalling the async executor
        // under concurrent autocomplete requests.
        let searcher = self.searcher.clone(); // cheap: Arc<IndexReader> clone
        let query = query.to_owned();

        tokio::task::spawn_blocking(move || searcher.search(&query, &filter, limit))
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

    async fn add_search_results(&self, results: Vec<SearchResult>) -> Result<(), AppError> {
        let mut w = self.writer.lock().await;

        for result in &results {
            // Delete existing doc by video_id to prevent duplicates
            if let Some(vid) = &result.youtube_video_id {
                let id_term = tantivy::Term::from_field_text(self.schema.youtube_video_id, vid);
                w.delete_term(id_term);
            }

            w.add_document(result.to_search_doc(&self.schema)).map_err(
                |e: tantivy::TantivyError| AppError::Search {
                    kind: SearchErrorKind::WriteFailed,
                    detail: e.to_string(),
                },
            )?;
        }

        w.commit()
            .map_err(|e: tantivy::TantivyError| AppError::Search {
                kind: SearchErrorKind::WriteFailed,
                detail: e.to_string(),
            })?;

        Ok(())
    }

    async fn delete_search_result(&self, video_id: &str) -> Result<(), AppError> {
        let mut w = self.writer.lock().await;
        let id_term = tantivy::Term::from_field_text(self.schema.youtube_video_id, video_id);
        w.delete_term(id_term);
        w.commit()
            .map_err(|e: tantivy::TantivyError| AppError::Search {
                kind: SearchErrorKind::WriteFailed,
                detail: e.to_string(),
            })?;
        Ok(())
    }
}
