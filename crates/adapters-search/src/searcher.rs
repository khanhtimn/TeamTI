use tantivy::{
    Index, IndexReader, ReloadPolicy, TantivyDocument,
    collector::TopDocs,
    query::{AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery},
    schema::{IndexRecordOption, Term, Value},
    tokenizer::TextAnalyzer,
};

use application::{AppError, SearchErrorKind};
use domain::track::TrackSummary;

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

    pub fn search(&self, raw_query: &str, limit: usize) -> Result<Vec<TrackSummary>, AppError> {
        let trimmed = raw_query.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }

        let tokens = self.tokenize(trimmed);
        if tokens.is_empty() {
            return Ok(vec![]);
        }

        let query = self.build_query(&tokens);
        tracing::debug!("{:?}", query);
        let searcher = self.reader.searcher();

        let top_docs = searcher
            .search(&*query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| AppError::Search {
                kind: SearchErrorKind::ReadFailed,
                detail: e.to_string(),
            })?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (_score, addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(addr).map_err(|e| AppError::Search {
                kind: SearchErrorKind::ReadFailed,
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

    /// Extract a `TrackSummary` from a retrieved Tantivy document.
    fn doc_to_summary(&self, doc: &TantivyDocument) -> Option<TrackSummary> {
        let s = &self.schema;

        let track_id = doc
            .get_first(s.track_id)?
            .as_str()?
            .parse::<uuid::Uuid>()
            .ok()?;

        let title = doc.get_first(s.title)?.as_str()?.to_owned();

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
