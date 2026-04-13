use tantivy::{
    Index, IndexReader, ReloadPolicy, TantivyDocument,
    collector::TopDocs,
    query::{AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery},
    schema::{IndexRecordOption, Term, Value},
    tokenizer::TextAnalyzer,
};

use application::{AppError, SearchErrorKind};
use domain::search::{SearchFilter, SearchResult};

use crate::{schema::MusicSchema, tokenizer::build_music_tokenizer};

/// Cloneable — cheap because IndexReader is Arc-backed internally.
#[derive(Clone)]
pub struct MusicSearcher {
    reader: IndexReader,
    schema: MusicSchema,
    tokenizer: TextAnalyzer,
}

impl MusicSearcher {
    pub fn new(index: &Index, schema: &MusicSchema) -> Result<Self, AppError> {
        let reader = index
            .reader_builder()
            // Searcher sees new documents within ~500ms of a writer commit.
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| AppError::Search {
                kind: SearchErrorKind::OpenFailed,
                detail: e.to_string(),
            })?;

        let tokenizer = build_music_tokenizer();
        index.tokenizers().register("music", tokenizer.clone());

        Ok(Self {
            reader,
            schema: schema.clone(),
            tokenizer,
        })
    }

    pub fn search(
        &self,
        raw_query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>, AppError> {
        let trimmed = raw_query.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }

        let tokens = self.tokenize(trimmed);
        if tokens.is_empty() {
            return Ok(vec![]);
        }

        let mut query = self.build_query(&tokens);

        if *filter == SearchFilter::YoutubeOnly {
            let mut should_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

            let term_youtube = Term::from_field_text(self.schema.source, "youtube");
            should_clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(term_youtube, IndexRecordOption::Basic)),
            ));

            let term_youtube_search = Term::from_field_text(self.schema.source, "youtube_search");
            should_clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    term_youtube_search,
                    IndexRecordOption::Basic,
                )),
            ));

            let filter_query = Box::new(BooleanQuery::new(should_clauses));

            query = Box::new(BooleanQuery::new(vec![
                (Occur::Must, query),
                (Occur::Must, filter_query),
            ]));
        } else if *filter == SearchFilter::LocalOnly {
            let term_tracks = Term::from_field_text(self.schema.source, "tracks");
            let filter_query = Box::new(TermQuery::new(term_tracks, IndexRecordOption::Basic));

            query = Box::new(BooleanQuery::new(vec![
                (Occur::Must, query),
                (Occur::Must, filter_query),
            ]));
        } else {
            // Filter is All. S2: apply a slight boost to local tracks so they outrank transient stubs
            let term_tracks = Term::from_field_text(self.schema.source, "tracks");
            let local_boost = Box::new(BoostQuery::new(
                Box::new(TermQuery::new(term_tracks, IndexRecordOption::Basic)),
                1.5,
            ));
            query = Box::new(BooleanQuery::new(vec![
                (Occur::Must, query),
                (Occur::Should, local_boost),
            ]));
        }

        tracing::debug!("{:?}", query);
        let searcher = self.reader.searcher();
        let play_count_field = self.schema.play_count;
        let top_docs = searcher
            .search(&*query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| AppError::Search {
                kind: SearchErrorKind::ReadFailed,
                detail: e.to_string(),
            })?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(addr).map_err(|e| AppError::Search {
                kind: SearchErrorKind::ReadFailed,
                detail: e.to_string(),
            })?;
            if let Some(summary) = self.doc_to_search_result(&doc) {
                // Post-process tweak using stored play_count to slightly weight popular results
                // O1: play count heuristic
                results.push((
                    summary,
                    doc.get_first(play_count_field)
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    score,
                ));
            }
        }

        // M2: Deterministic tie-breaking for stable autocomplete ordering.
        // 1. Higher score first
        // 2. Higher play_count (local tracks with listens outrank stubs)
        // 3. Source priority: tracks > youtube > youtube_search
        // 4. Stable ID for absolute determinism
        results.sort_by(|a, b| {
            // Primary: Tantivy score
            if (a.2 - b.2).abs() > 0.01 {
                return b.2.partial_cmp(&a.2).unwrap();
            }
            // Secondary: play count
            if a.1 != b.1 {
                return b.1.cmp(&a.1);
            }
            // Tertiary: source priority
            let source_order = |s: &str| -> u8 {
                match s {
                    "tracks" => 0,
                    "youtube" => 1,
                    "youtube_search" => 2,
                    _ => 3,
                }
            };
            let sa = source_order(&a.0.source);
            let sb = source_order(&b.0.source);
            if sa != sb {
                return sa.cmp(&sb);
            }
            // Quaternary: stable ID sort
            let id_a =
                a.0.track_id
                    .map(|t| t.to_string())
                    .or_else(|| a.0.youtube_video_id.clone())
                    .unwrap_or_default();
            let id_b =
                b.0.track_id
                    .map(|t| t.to_string())
                    .or_else(|| b.0.youtube_video_id.clone())
                    .unwrap_or_default();
            id_a.cmp(&id_b)
        });

        Ok(results.into_iter().map(|(r, _, _)| r).collect())
    }

    /// Build the full compound query for a tokenized input.
    ///
    /// Strategy:
    ///   - Interior tokens (all except last):  FuzzyTermQuery(distance=1)
    ///   - Last token:                         FuzzyTermQuery::new_prefix(distance=1)
    ///   - Each token produces a Should-BooleanQuery across all fields,
    ///     each field wrapped in a BoostQuery.
    ///   - All per-token sub-queries are combined with Must semantics:
    ///     every token must match somewhere in the document.
    fn build_query(&self, tokens: &[String]) -> Box<dyn Query> {
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
        let fields: Vec<(tantivy::schema::Field, f32)> = vec![
            (self.schema.title, 4.0),
            (self.schema.artist, 3.0),
            (self.schema.album, 2.0),
            (self.schema.composer, 1.5),
            (self.schema.genre, 1.0),
            (self.schema.lyricist, 0.8),
        ];

        let mut should: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(fields.len());

        let use_fuzzy = token.chars().count() >= 4;

        for (field, boost) in fields {
            let term = Term::from_field_text(field, token);

            // transposition_cost_one = true:
            //   "teh" → "the" counts as distance 1 (transposition),
            //   not distance 2 (delete + insert). More permissive.
            let base_query: Box<dyn Query> = match (is_last, use_fuzzy) {
                // Long last token: prefix + fuzzy distance=1
                (true, true) => Box::new(FuzzyTermQuery::new_prefix(term, 1, true)),
                // Short last token: pure prefix, no fuzzy
                (true, false) => Box::new(FuzzyTermQuery::new_prefix(term, 0, true)),
                // Long interior token: fuzzy distance=1
                (false, true) => Box::new(FuzzyTermQuery::new(term, 1, true)),
                // Short interior token: exact match
                (false, false) => Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            };

            let boosted = Box::new(BoostQuery::new(base_query, boost));
            should.push((Occur::Should, boosted));
        }

        Box::new(BooleanQuery::new(should))
    }

    /// Extract a `SearchResult` from a retrieved Tantivy document.
    fn doc_to_search_result(&self, doc: &TantivyDocument) -> Option<SearchResult> {
        let s = &self.schema;

        let source = doc.get_first(s.source)?.as_str()?.to_owned();

        let track_id_str = doc.get_first(s.track_id)?.as_str()?;
        let track_id = if track_id_str.is_empty() {
            None
        } else {
            track_id_str.parse::<uuid::Uuid>().ok()
        };

        let title = doc.get_first(s.title)?.as_str()?.to_owned();

        let artist_display = doc
            .get_first(s.artist)
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let uploader = doc
            .get_first(s.uploader)
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let youtube_video_id = doc
            .get_first(s.youtube_video_id)
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let duration_ms = doc
            .get_first(s.duration_ms)
            .and_then(|v| v.as_u64())
            .map(u64::cast_signed)
            .filter(|&v| v > 0);

        Some(SearchResult {
            source,
            track_id,
            youtube_video_id,
            title,
            artist_display,
            uploader,
            duration_ms,
        })
    }

    /// Tokenize a raw query string using the internal tokenizer pipeline.
    fn tokenize(&self, text: &str) -> Vec<String> {
        let mut tokenizer = self.tokenizer.clone();
        let mut stream = tokenizer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        tokens
    }
}
