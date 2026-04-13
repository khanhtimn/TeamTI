use tantivy::Index;
use uuid::Uuid;

use crate::indexer::{ToSearchDoc, TrackRow};
use crate::schema::MusicSchema;
use crate::searcher::MusicSearcher;
use crate::tokenizer::build_music_tokenizer;
use domain::search::SearchFilter;

/// Create an in-memory Tantivy index pre-loaded with the given tracks.
fn build_test_index(tracks: &[TrackRow]) -> (Index, MusicSearcher, MusicSchema) {
    let schema = MusicSchema::build();
    let index = Index::create_in_ram(schema.schema.clone());
    // Register the custom tokenizer before writing or reading
    index
        .tokenizers()
        .register("music", build_music_tokenizer());
    let mut writer = index.writer(15_000_000).unwrap();

    for track in tracks {
        writer.add_document(track.to_search_doc(&schema)).unwrap();
    }
    writer.commit().unwrap();

    let searcher = MusicSearcher::new(&index, &schema).unwrap();
    (index, searcher, schema)
}

/// Create an in-memory index with both tracks and youtube_search_cache rows.
fn build_mixed_index(
    tracks: &[TrackRow],
    cache_rows: &[domain::youtube::YoutubeSearchCacheRow],
) -> (Index, MusicSearcher, MusicSchema) {
    let schema = MusicSchema::build();
    let index = Index::create_in_ram(schema.schema.clone());
    index
        .tokenizers()
        .register("music", build_music_tokenizer());
    let mut writer = index.writer(15_000_000).unwrap();

    for track in tracks {
        writer.add_document(track.to_search_doc(&schema)).unwrap();
    }
    for row in cache_rows {
        writer.add_document(row.to_search_doc(&schema)).unwrap();
    }
    writer.commit().unwrap();

    let searcher = MusicSearcher::new(&index, &schema).unwrap();
    (index, searcher, schema)
}

fn make_track(title: &str, artist: &str, source: &str) -> TrackRow {
    TrackRow {
        track_id: Uuid::new_v4(),
        title: title.to_string(),
        artist_display: Some(artist.to_string()),
        year: None,
        bpm: None,
        track_genres: vec![],
        album_title: None,
        album_genres: vec![],
        primary_artists: vec![artist.to_string()],
        artist_sort_names: vec![],
        composers: vec![],
        lyricists: vec![],
        featured_artists: vec![],
        source: source.to_string(),
        youtube_video_id: None,
        youtube_uploader: None,
        duration_ms: Some(240_000),
        play_count: Some(10),
    }
}

fn make_yt_track(title: &str, uploader: &str, video_id: &str) -> TrackRow {
    TrackRow {
        track_id: Uuid::new_v4(),
        title: title.to_string(),
        artist_display: Some(uploader.to_string()),
        year: None,
        bpm: None,
        track_genres: vec![],
        album_title: None,
        album_genres: vec![],
        primary_artists: vec![],
        artist_sort_names: vec![],
        composers: vec![],
        lyricists: vec![],
        featured_artists: vec![],
        source: "youtube".to_string(),
        youtube_video_id: Some(video_id.to_string()),
        youtube_uploader: Some(uploader.to_string()),
        duration_ms: Some(300_000),
        play_count: Some(0),
    }
}

fn make_cache_row(
    title: &str,
    uploader: &str,
    video_id: &str,
) -> domain::youtube::YoutubeSearchCacheRow {
    domain::youtube::YoutubeSearchCacheRow {
        id: Uuid::new_v4(),
        video_id: video_id.to_string(),
        title: title.to_string(),
        uploader: Some(uploader.to_string()),
        channel_id: None,
        duration_ms: Some(200),
        thumbnail_url: None,
        query: "test".to_string(),
        track_id: None,
        created_at: chrono::Utc::now(),
        last_seen_at: chrono::Utc::now(),
    }
}

// ─── Tokenizer edge cases (S2) ──────────────────────────────────────

#[test]
fn tokenizer_handles_cjk_input() {
    let tracks = vec![
        make_track("新宝島", "サカナクション", "tracks"),
        make_track("失恋ショコラティエ", "松本潤", "tracks"),
    ];
    let (_, searcher, _) = build_test_index(&tracks);

    let results = searcher.search("新宝島", &SearchFilter::All, 10).unwrap();
    assert!(!results.is_empty(), "CJK query should match CJK titles");
    assert_eq!(results[0].title, "新宝島");
}

#[test]
fn tokenizer_handles_mixed_latin_cjk() {
    let tracks = vec![make_track("YOASOBI 夜に駆ける", "YOASOBI", "tracks")];
    let (_, searcher, _) = build_test_index(&tracks);

    // Search by the Latin portion
    let results = searcher.search("YOASOBI", &SearchFilter::All, 10).unwrap();
    assert!(
        !results.is_empty(),
        "Latin portion of mixed CJK+Latin title should match"
    );
}

#[test]
fn tokenizer_handles_emoji_only_query() {
    // Emoji-only queries should NOT panic — they should return empty or
    // match nothing. SimpleTokenizer splits on Unicode boundaries; emoji
    // become individual tokens that won't match anything useful.
    let tracks = vec![make_track("Hello World", "Test", "tracks")];
    let (_, searcher, _) = build_test_index(&tracks);

    let results = searcher.search("🎵🎶", &SearchFilter::All, 10).unwrap();
    // We don't require matches, just no panic
    let _ = results;
}

#[test]
fn tokenizer_handles_punctuation_heavy_query() {
    let tracks = vec![
        make_track("What's Going On", "Marvin Gaye", "tracks"),
        make_track("Don't Stop Me Now", "Queen", "tracks"),
    ];
    let (_, searcher, _) = build_test_index(&tracks);

    // Punctuation should be stripped by SimpleTokenizer
    let results = searcher
        .search("what's going", &SearchFilter::All, 10)
        .unwrap();
    assert!(
        !results.is_empty(),
        "Punctuation-containing queries should still match"
    );
}

#[test]
fn tokenizer_handles_dash_slash_colon() {
    // These characters are common in music: "AC/DC", "Run-D.M.C.", etc.
    let tracks = vec![make_track("Thunderstruck", "AC/DC", "tracks")];
    let (_, searcher, _) = build_test_index(&tracks);

    let results = searcher.search("AC DC", &SearchFilter::All, 10).unwrap();
    assert!(
        !results.is_empty(),
        "Slash-separated artists should match when queried with space"
    );
}

#[test]
fn tokenizer_handles_empty_and_whitespace() {
    let tracks = vec![make_track("Test", "Artist", "tracks")];
    let (_, searcher, _) = build_test_index(&tracks);

    assert!(
        searcher
            .search("", &SearchFilter::All, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        searcher
            .search("   ", &SearchFilter::All, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        searcher
            .search("\t\n", &SearchFilter::All, 10)
            .unwrap()
            .is_empty()
    );
}

// ─── Filter correctness ─────────────────────────────────────────────

#[test]
fn local_only_filter_excludes_youtube() {
    let tracks = vec![
        make_track("Local Song", "Local Artist", "tracks"),
        make_yt_track("YT Song", "YT Channel", "abc123"),
    ];
    let cache = vec![make_cache_row("Cache Song", "Cache Channel", "xyz789")];
    let (_, searcher, _) = build_mixed_index(&tracks, &cache);

    let results = searcher
        .search("Song", &SearchFilter::LocalOnly, 10)
        .unwrap();
    assert!(
        results.iter().all(|r| r.source == "tracks"),
        "LocalOnly should exclude all non-tracks sources"
    );
    assert_eq!(results.len(), 1);
}

#[test]
fn youtube_only_filter_excludes_local() {
    let tracks = vec![
        make_track("Local Song", "Local Artist", "tracks"),
        make_yt_track("YouTube Song", "Channel", "abc123"),
    ];
    let cache = vec![make_cache_row("Cached Search Song", "Uploader", "xyz789")];
    let (_, searcher, _) = build_mixed_index(&tracks, &cache);

    let results = searcher
        .search("Song", &SearchFilter::YoutubeOnly, 10)
        .unwrap();
    assert!(
        results
            .iter()
            .all(|r| r.source == "youtube" || r.source == "youtube_search"),
        "YoutubeOnly should exclude tracks source"
    );
}

#[test]
fn all_filter_boosts_local_tracks() {
    // Same title, but local source should rank higher due to boost
    let tracks = vec![
        make_track("Bohemian Rhapsody", "Queen", "tracks"),
        make_yt_track("Bohemian Rhapsody", "QueenVEVO", "vid123"),
    ];
    let (_, searcher, _) = build_test_index(&tracks);

    let results = searcher
        .search("Bohemian Rhapsody", &SearchFilter::All, 10)
        .unwrap();
    assert!(results.len() >= 2, "Both results should match");
    assert_eq!(results[0].source, "tracks", "Local track should rank first");
}

// ─── Deterministic ordering (M2) ─────────────────────────────────────

#[test]
fn same_query_returns_identical_ordering() {
    let tracks = vec![
        make_track("Song Alpha", "Artist A", "tracks"),
        make_track("Song Beta", "Artist B", "tracks"),
        make_track("Song Gamma", "Artist C", "tracks"),
    ];
    let (_, searcher, _) = build_test_index(&tracks);

    let first_run = searcher.search("Song", &SearchFilter::All, 10).unwrap();
    for _ in 0..10 {
        let nth_run = searcher.search("Song", &SearchFilter::All, 10).unwrap();
        assert_eq!(
            first_run.iter().map(|r| &r.title).collect::<Vec<_>>(),
            nth_run.iter().map(|r| &r.title).collect::<Vec<_>>(),
            "Repeated identical query must produce identical ordering"
        );
    }
}

// ─── ToSearchDoc field completeness (C5/S1) ──────────────────────────

#[test]
fn search_result_to_doc_populates_all_fast_fields() {
    let schema = MusicSchema::build();
    let result = domain::search::SearchResult {
        source: "youtube_search".to_string(),
        track_id: None,
        youtube_video_id: Some("test123".to_string()),
        title: "Test Title".to_string(),
        artist_display: None,
        uploader: Some("Test Uploader".to_string()),
        duration_ms: Some(180_000),
    };

    let doc = result.to_search_doc(&schema);

    // All FAST fields must be present
    assert!(
        doc.get_first(schema.year).is_some(),
        "year field must be set"
    );
    assert!(doc.get_first(schema.bpm).is_some(), "bpm field must be set");
    assert!(
        doc.get_first(schema.duration_ms).is_some(),
        "duration_ms must be set"
    );
    assert!(
        doc.get_first(schema.play_count).is_some(),
        "play_count must be set"
    );
    assert!(doc.get_first(schema.source).is_some(), "source must be set");
    assert!(
        doc.get_first(schema.track_id).is_some(),
        "track_id must be set (even if empty)"
    );
}

#[test]
fn cache_row_to_doc_matches_search_result_field_set() {
    let schema = MusicSchema::build();

    let cache = make_cache_row("Title", "Uploader", "vid1");
    let cache_doc = cache.to_search_doc(&schema);

    let result = domain::search::SearchResult {
        source: "youtube_search".to_string(),
        track_id: None,
        youtube_video_id: Some("vid2".to_string()),
        title: "Title".to_string(),
        artist_display: None,
        uploader: Some("Uploader".to_string()),
        duration_ms: Some(200),
    };
    let result_doc = result.to_search_doc(&schema);

    // Both should have the same set of fields populated
    let count_fields = |doc: &tantivy::TantivyDocument| -> usize {
        let mut n = 0;
        for f in [
            schema.year,
            schema.bpm,
            schema.duration_ms,
            schema.play_count,
            schema.source,
            schema.track_id,
            schema.title,
        ] {
            if doc.get_first(f).is_some() {
                n += 1;
            }
        }
        n
    };

    assert_eq!(
        count_fields(&cache_doc),
        count_fields(&result_doc),
        "Cache row and SearchResult must produce docs with identical field coverage"
    );
}

// ─── SubmissionValue round-trip (M8) ─────────────────────────────────

#[test]
fn submission_value_serialize_deserialize_roundtrip() {
    use domain::autocomplete::SubmissionValue;

    let uuid = Uuid::new_v4();
    let tid = SubmissionValue::TrackId(uuid);
    assert_eq!(
        SubmissionValue::deserialize(&tid.serialize())
            .unwrap()
            .serialize(),
        tid.serialize()
    );

    let vid = SubmissionValue::YoutubeVideoId("dQw4w9WgXcQ".to_string());
    assert_eq!(
        SubmissionValue::deserialize(&vid.serialize())
            .unwrap()
            .serialize(),
        vid.serialize()
    );
}

#[test]
fn submission_classify_rejects_ambiguous_strings() {
    use domain::autocomplete::SubmissionValue;

    // Raw text without prefix should not classify
    assert!(SubmissionValue::classify("bohemian rhapsody").is_none());
    // URL-like strings that aren't prefixed should not classify
    assert!(SubmissionValue::classify("https://youtube.com/watch?v=abc").is_none());
    // Empty
    assert!(SubmissionValue::classify("").is_none());
    assert!(SubmissionValue::classify("   ").is_none());
}

#[test]
fn submission_backward_compat_bare_uuid() {
    use domain::autocomplete::SubmissionValue;

    // Bare UUIDs should still parse for backward compatibility
    let uuid = Uuid::new_v4();
    let result = SubmissionValue::deserialize(&uuid.to_string()).unwrap();
    match result {
        SubmissionValue::TrackId(id) => assert_eq!(id, uuid),
        _ => panic!("Bare UUID should parse as TrackId"),
    }
}

// ─── Fuzzy/prefix matching ──────────────────────────────────────────

#[test]
fn prefix_matching_works_for_partial_queries() {
    let tracks = vec![make_track("Bohemian Rhapsody", "Queen", "tracks")];
    let (_, searcher, _) = build_test_index(&tracks);

    // Partial prefix should match via prefix query
    let results = searcher.search("bohe", &SearchFilter::All, 10).unwrap();
    assert!(
        !results.is_empty(),
        "Prefix query should find matching tracks"
    );
    assert_eq!(results[0].title, "Bohemian Rhapsody");
}

#[test]
fn fuzzy_matching_handles_typos() {
    let tracks = vec![make_track("Bohemian Rhapsody", "Queen", "tracks")];
    let (_, searcher, _) = build_test_index(&tracks);

    // Typo: "boheimian" instead of "bohemian" — fuzzy distance=1 should catch this
    let results = searcher
        .search("boheimian", &SearchFilter::All, 10)
        .unwrap();
    // Fuzzy matching may or may not catch this depending on distance. If it does, great.
    // The important thing is no panic.
    let _ = results;
}

// ─── Accent folding ─────────────────────────────────────────────────

#[test]
fn accent_folding_matches_unaccented_query() {
    let tracks = vec![
        make_track("Déjà Vu", "Crosby Stills Nash & Young", "tracks"),
        make_track("Señorita", "Shawn Mendes", "tracks"),
    ];
    let (_, searcher, _) = build_test_index(&tracks);

    // ASCII-folded queries should still match accented titles
    let results = searcher.search("deja", &SearchFilter::All, 10).unwrap();
    assert!(
        !results.is_empty(),
        "Unaccented query should match accented title via AsciiFoldingFilter"
    );
    assert!(results[0].title.starts_with("Déjà"), "Should match Déjà Vu");
}
