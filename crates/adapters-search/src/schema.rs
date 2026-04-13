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

    pub uploader: Field, // STRING     · STORED

    // ── Identifier ───────────────────────────────────────────────
    pub track_id: Field,         // STRING · STORED
    pub youtube_video_id: Field, // STRING · STORED

    // ── Fast fields (scoring / filtering, not text-matched) ──────
    pub year: Field,        // u64 · FAST
    pub bpm: Field,         // u64 · FAST
    pub duration_ms: Field, // u64 · FAST · STORED
    pub play_count: Field,  // u64 · FAST

    // ── Source indicator ─────────────────────────────────────────
    pub source: Field, // STRING · STORED
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
        let uploader = b.add_text_field("uploader", STRING | STORED);

        let track_id = b.add_text_field("track_id", STRING | STORED);
        let youtube_video_id = b.add_text_field("youtube_video_id", STRING | STORED);

        let year = b.add_u64_field("year", fast_u64.clone());
        let bpm = b.add_u64_field("bpm", fast_u64.clone());

        let fast_u64_stored = NumericOptions::default().set_stored() | FAST;
        let duration_ms = b.add_u64_field("duration_ms", fast_u64_stored);
        let play_count = b.add_u64_field("play_count", fast_u64.clone());

        let source = b.add_text_field("source", STRING | STORED);
        let schema = b.build();

        Self {
            schema,
            title,
            artist,
            album,
            genre,
            composer,
            lyricist,
            uploader,
            track_id,
            youtube_video_id,
            year,
            bpm,
            duration_ms,
            play_count,
            source,
        }
    }
}
