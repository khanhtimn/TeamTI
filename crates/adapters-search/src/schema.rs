use tantivy::schema::{
    FAST, Field, IndexRecordOption, NumericOptions, STORED, STRING, Schema, SchemaBuilder,
    TextFieldIndexing, TextOptions,
};

/// All field handles for the music index, derived from a single Schema.
/// Constructed once at index open/creation and shared via Arc.
#[derive(Clone, Debug)]
pub struct MusicSchema {
    pub schema: Schema,

    // ── Full-text fields ──────────────────────────────────────────
    pub title: Field,    // music tokenizer · WithFreqsAndPositions · STORED
    pub artist: Field,   // music tokenizer · WithFreqsAndPositions · STORED
    pub album: Field,    // music tokenizer · WithFreqs              · STORED
    pub genre: Field,    // music tokenizer · WithFreqs              (not stored)
    pub composer: Field, // music tokenizer · WithFreqs              (not stored)
    pub lyricist: Field, // music tokenizer · WithFreqs              (not stored)

    // ── Identifier ───────────────────────────────────────────────
    pub track_id: Field, // STRING · STORED

    // ── Fast fields (scoring / filtering, not text-matched) ──────
    pub year: Field, // u64 · FAST
    pub bpm: Field,  // u64 · FAST
}

impl MusicSchema {
    #[must_use]
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

        let with_freqs = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("music")
                .set_index_option(IndexRecordOption::WithFreqs),
        );
        // NOT stored — genre/composer/lyricist are not shown in autocomplete

        let fast_u64 = NumericOptions::default() | FAST;

        let title = b.add_text_field("title", with_positions.clone());
        let artist = b.add_text_field("artist", with_positions);
        let album = b.add_text_field("album", with_freqs_stored);
        let genre = b.add_text_field("genre", with_freqs.clone());
        let composer = b.add_text_field("composer", with_freqs.clone());
        let lyricist = b.add_text_field("lyricist", with_freqs);
        let track_id = b.add_text_field("track_id", STRING | STORED);
        let year = b.add_u64_field("year", fast_u64.clone());
        let bpm = b.add_u64_field("bpm", fast_u64);
        let schema = b.build();

        Self {
            schema,
            title,
            artist,
            album,
            genre,
            composer,
            lyricist,
            track_id,
            year,
            bpm,
        }
    }
}
