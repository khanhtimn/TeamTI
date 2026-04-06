use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer};

pub fn build_music_tokenizer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        // No stemmer — proper nouns (artist/album/track names) must
        // not be stemmed. "Portishead" != "Port". See v3 design notes.
        // No stop words — "The" is part of "The Beatles", "The National".
        .build()
}
