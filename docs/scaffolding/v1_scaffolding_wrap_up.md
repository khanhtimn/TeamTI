════════════════════════════════════════════════════════════════════════════════
TEAMTI — V1 WRAP-UP PROMPT
Goal: close all known gaps and produce a working bot ready for v2
════════════════════════════════════════════════════════════════════════════════

You are working inside the `teamti` Rust Cargo workspace. The v1 scaffold is
already in place and compiles cleanly. Your job is to implement the remaining
gaps so that v1 is functionally complete: join voice, leave voice, stream audio,
and search/queue audio by title.

Do not change the workspace structure, crate boundaries, or dependency direction.
Do not add Poise. Do not add an event bus. Do not add web portal logic.

Read this entire document before writing any code.

════════════════════════════════════════════════════════════════════════════════
SECTION 1 — MANDATORY API REFERENCE (serenity `next` + songbird `serenity-next`)
════════════════════════════════════════════════════════════════════════════════

This workspace tracks development branches. Do NOT use docs.rs — it reflects
stable releases only. Read source directly:

  EventHandler trait  : https://github.com/serenity-rs/serenity/blob/next/src/gateway/client/event_handler.rs
  FullEvent enum      : https://github.com/serenity-rs/serenity/blob/next/src/model/event.rs
  ClientBuilder       : https://github.com/serenity-rs/serenity/blob/next/src/gateway/client/mod.rs
  Token type          : https://github.com/serenity-rs/serenity/blob/next/src/secrets.rs
  CreateCommand       : https://github.com/serenity-rs/serenity/blob/next/src/builder/create_command.rs
  Songbird example    : https://github.com/serenity-rs/songbird/blob/serenity-next/examples/serenity/voice/src/main.rs

BREAKING API RULES — violating any of these will cause compile failure:

1. EventHandler uses ONE method only:
     async fn dispatch(&self, ctx: &Context, event: &FullEvent)
   ctx is &Context (reference). No separate ready/interaction_create methods.

2. FullEvent variants are #[non_exhaustive]. Every match arm must include `..`.

3. CreateCommand and CreateCommandOption have lifetime parameters:
     pub fn my_cmd() -> CreateCommand<'static> { ... }

4. set_commands takes a slice reference:
     guild_id.set_commands(http, &[ping(), join(), leave(), play(), scan()]).await?;

5. Client::builder takes Token type, not &str:
     let token: Token = config.discord_token.parse()?;

6. Use .voice_manager(songbird) not .voice_manager_arc().

7. rustls crypto provider must be the very first line of main():
     rustls::crypto::ring::default_provider()
         .install_default()
         .expect("Failed to install rustls crypto provider");

8. For autocomplete, the interaction variant is Interaction::Autocomplete(ac).
   Respond with ac.create_response(http, CreateAutocompleteResponse::new()
       .set_choices(vec![...])).await.
   Verify exact type names in the serenity next source before writing.

9. serenity::prelude::* re-exports: Client, Context, EventHandler,
   GatewayIntents, Token, Mentionable, and error types.

════════════════════════════════════════════════════════════════════════════════
SECTION 2 — CURRENT STATE SUMMARY
════════════════════════════════════════════════════════════════════════════════

What already works (do not break):
  - cargo check --workspace passes cleanly
  - /ping, /join, /leave, /play_local commands registered and dispatched
  - Voice join/leave/play via Songbird builtin-queue
  - Postgres connection, SQLx compile-time macros, migration runner
  - adapters-media-store copies files and mints ManagedBlobRef
  - adapters-persistence MediaRepository, PlaylistRepository, etc.
  - tracing initialized, dotenvy config loading

Known gaps to fix in this pass (details in Section 3):
  1. Importer is not idempotent — re-queuing a file creates duplicate copies and rows
  2. No media scanner — the bot has no way to discover and catalog local files
  3. /play_local accepts a raw filesystem path from the user (wrong UX, wrong security)
  4. No Discord autocomplete for track search
  5. search_vector uses 'english' config — broken for non-English music metadata
  6. No MediaSearchPort abstraction — search is coupled directly to persistence
  7. /scan command does not exist

════════════════════════════════════════════════════════════════════════════════
SECTION 3 — WHAT TO IMPLEMENT
════════════════════════════════════════════════════════════════════════════════

──────────────────────────────────────────────────────────────
3A. DATABASE MIGRATIONS
──────────────────────────────────────────────────────────────

Do NOT modify migration 0001.
Add two new migrations in order.

── Migration 0002: add content_hash and artist columns ──

File: migrations/0002_add_content_hash_artist.sql

  ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS content_hash TEXT;
  ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS original_filename TEXT;
  ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS artist TEXT;

  CREATE UNIQUE INDEX IF NOT EXISTS idx_media_assets_content_hash
      ON media_assets(content_hash)
      WHERE content_hash IS NOT NULL;

── Migration 0003: multilingual search ──

File: migrations/0003_multilingual_search.sql

  -- Enable required bundled extensions (no external install needed)
  CREATE EXTENSION IF NOT EXISTS unaccent;
  CREATE EXTENSION IF NOT EXISTS pg_trgm;

  -- Custom text search config: simple tokenization + unaccent normalization.
  -- Rationale: 'english' config stems incorrectly for non-English titles,
  -- drops valid stop words like 'wo', 'ni', 'de', and produces zero tokens
  -- for CJK text. 'simple' preserves all tokens; unaccent handles diacritics.
  CREATE TEXT SEARCH DICTIONARY IF NOT EXISTS unaccent_simple (
      TEMPLATE = unaccent,
      RULES    = 'unaccent'
  );

  CREATE TEXT SEARCH CONFIGURATION IF NOT EXISTS music_simple (COPY = simple);

  ALTER TEXT SEARCH CONFIGURATION music_simple
      ALTER MAPPING FOR hword, hword_part, word
      WITH unaccent_simple, simple;

  -- Normalized plaintext column: lowercased, unaccented, all fields joined.
  -- Used by both the tsvector and the trigram index.
  ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS search_text TEXT
      GENERATED ALWAYS AS (
          lower(
              unaccent(
                  coalesce(title, '')
                  || ' ' || coalesce(artist, '')
                  || ' ' || coalesce(original_filename, '')
              )
          )
      ) STORED;

  -- FTS vector using music_simple (language-agnostic, diacritics normalized)
  ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS search_vector tsvector
      GENERATED ALWAYS AS (
          to_tsvector('music_simple',
              coalesce(title, '')
              || ' ' || coalesce(artist, '')
              || ' ' || coalesce(original_filename, '')
          )
      ) STORED;

  -- GIN index for tsvector FTS queries
  CREATE INDEX IF NOT EXISTS idx_media_assets_search
      ON media_assets USING GIN(search_vector);

  -- GIN trigram index for fuzzy, substring, and CJK partial matching
  CREATE INDEX IF NOT EXISTS idx_media_assets_trgm
      ON media_assets USING GIN(search_text gin_trgm_ops);

Both migrations must run at startup via sqlx::migrate! as before.

──────────────────────────────────────────────────────────────
3B. DOMAIN MODEL CHANGES
──────────────────────────────────────────────────────────────

File: crates/domain/src/media.rs

Add to MediaAsset:
  pub content_hash: Option<String>,
  pub artist: Option<String>,
  pub search_text: Option<String>,   // generated, read-only, populated by DB

MediaAsset must remain free of Serenity, Songbird, SQLx, and filesystem types.

──────────────────────────────────────────────────────────────
3C. APPLICATION LAYER — MediaSearchPort
──────────────────────────────────────────────────────────────

File: crates/application/src/ports/search.rs

Define this port:

  #[async_trait]
  pub trait MediaSearchPort: Send + Sync {
      /// Search media assets by title/artist/filename.
      /// Returns up to `limit` results ordered by relevance.
      async fn search_assets(
          &self,
          query: &str,
          limit: usize,
      ) -> Result<Vec<MediaSearchResult>, AppError>;
  }

  pub struct MediaSearchResult {
      pub asset_id: Uuid,
      pub title: String,
      pub artist: Option<String>,
      pub original_filename: Option<String>,
  }

Expose this port in crates/application/src/ports/mod.rs.

The EnqueueTrack service must accept an asset_id (Uuid), not a path string.
Update application/src/services/enqueue_track.rs accordingly:
  - Input: EnqueueTrackRequest { guild_id: GuildId, asset_id: Uuid, requested_by: UserId }
  - Resolve asset via MediaRepository::find_by_id(asset_id)
  - Return error if not found
  - Pass resolved blob path to PlaybackGateway

──────────────────────────────────────────────────────────────
3D. PERSISTENCE — MediaSearchPort implementation
──────────────────────────────────────────────────────────────

File: crates/adapters-persistence/src/repositories/media_repository.rs

Implement MediaSearchPort for the existing Postgres repository.

Use this hybrid search query that combines FTS and trigram for best multilingual
coverage:

  SELECT
      id,
      title,
      artist,
      original_filename,
      GREATEST(
          ts_rank_cd(search_vector, websearch_to_tsquery('music_simple', $1)),
          word_similarity($2, search_text)
      ) AS rank
  FROM media_assets
  WHERE
      search_vector @@ websearch_to_tsquery('music_simple', $1)
      OR ($2 <% search_text)
  ORDER BY rank DESC
  LIMIT $3;

Note: $1 = $2 = user query string. $3 = limit as i64.
The <% operator is word_similarity threshold (trigram), index-supported by
the GIN trigram index. The websearch_to_tsquery function is safe with arbitrary
user input — it does not throw on malformed queries.

Also add to MediaRepository:
  find_by_content_hash(hash: &str) -> Result<Option<MediaAsset>, AppError>
  find_by_id(id: Uuid) -> Result<Option<MediaAsset>, AppError>

──────────────────────────────────────────────────────────────
3E. MEDIA STORE — Idempotent importer + Scanner
──────────────────────────────────────────────────────────────

Add dependency to adapters-media-store/Cargo.toml:
  blake3 = "1"
  walkdir = "2"
  symphonia with features: mp3, aac, isomp4, alac, ogg, vorbis, wav, flac

── Idempotent Importer ──

File: crates/adapters-media-store/src/importer.rs

The importer runs when an audio file needs to enter the catalog.
It must be idempotent: importing the same file twice produces one asset.

Algorithm:
  1. Compute BLAKE3 hash of the source file bytes (hex-encode it).
  2. Query Postgres via MediaRepository::find_by_content_hash(hash).
  3. If found: return the existing MediaAsset. Stop here. No copy, no insert.
  4. If not found:
     a. For UPLOAD sources: copy file to managed store root as {UUID}_{filename}.
        ManagedBlobRef points to the managed store path.
     b. For LOCAL/SCAN sources: do NOT copy the file.
        ManagedBlobRef points to the original absolute filesystem path.
     c. Extract metadata (see below).
     d. Insert MediaAsset row with content_hash, blob_location, metadata.
     e. Return the new asset.

Metadata extraction (using symphonia):
  - Probe the file with symphonia's default probe.
  - If probe succeeds:
    - Read title tag (Tag::known_key == StandardTagKey::TrackTitle).
    - Read artist tag (StandardTagKey::Artist).
    - Compute duration_ms from default track TimeBase and n_frames if available.
  - If symphonia probe fails: set title = filename stem, artist = None,
    duration_ms = None. Log a warning. Do not abort.

Filename heuristic as fallback (only when symphonia finds no tags):
  - If filename contains " - ": left side = artist, right side = title.
  - Otherwise: full stem = title, artist = None.
  - Strip extension, trim whitespace.

── Media Scanner ──

File: crates/adapters-media-store/src/scanner.rs

The scanner discovers and catalogs all audio files under a configured root
directory. It is the correct and only entry point for registering local media.
It must NOT copy files — local files keep their original paths as blob refs.

pub struct ScanReport {
    pub total_found: usize,
    pub newly_registered: usize,
    pub skipped_existing: usize,
    pub errors: Vec<String>,
}

Supported extensions: mp3, flac, ogg, wav, aac, m4a, opus

Algorithm:
  1. Walk media_root recursively using walkdir, follow symlinks: false.
  2. For each file with a supported audio extension:
     a. Compute BLAKE3 hash.
     b. Check Postgres via find_by_content_hash.
     c. If exists: increment skipped_existing. Log at trace level.
     d. If new: call importer with MediaOrigin::Local and the file path.
        On success: increment newly_registered.
        On error: push error message, increment errors, continue scan.
  3. Return ScanReport.

The scanner must accept:
  - &self
  - media_root: &Path
  - repo: &dyn MediaRepository (or Arc<dyn MediaRepository>)

It must be callable from:
  - apps/bot/src/main.rs on startup (before command registration)
  - The /scan slash command handler

──────────────────────────────────────────────────────────────
3F. SHARED CONFIG
──────────────────────────────────────────────────────────────

File: crates/shared-config/src/lib.rs

Add:
  pub media_root: PathBuf,

Read from env var: MEDIA_ROOT

──────────────────────────────────────────────────────────────
3G. DISCORD ADAPTER — Commands
──────────────────────────────────────────────────────────────

Replace the existing /play_local command with /play.
Remove raw path input. Use autocomplete-backed asset search.

── /play command ──

Definition:
  CreateCommand::new("play")
      .description("Search and queue a track")
      .add_option(
          CreateCommandOption::new(
              CommandOptionType::String,
              "query",
              "Track title to search"
          )
          .required(true)
          .set_autocomplete(true)
      )

Autocomplete handler (in dispatch(), Interaction::Autocomplete branch):
  - Read focused option value from autocomplete interaction data.
  - If query is empty or less than 2 characters: return empty choices list.
  - Call MediaSearchPort::search_assets(query, 25).
  - Map results to autocomplete choices:
      display name: "{title} — {artist}" if artist present, else "{title}"
      value: asset_id.to_string()
  - Respond with CreateAutocompleteResponse.

Command handler (in dispatch(), Interaction::Command branch, command name "play"):
  - Read the query option value (this is the asset_id string the user selected).
  - Parse as Uuid. If invalid: respond with ephemeral error "Invalid selection".
  - Resolve the invoking member's current voice channel from guild cache.
  - If not in voice: respond ephemeral "You must be in a voice channel".
  - Call EnqueueTrackRequest { guild_id, asset_id, requested_by }.
  - On success: respond with "▶ Queued: {title} by {artist}".
  - On error: respond ephemeral with a clean message. Do not expose internal paths.

── /scan command ──

  CreateCommand::new("scan")
      .description("Scan and index local media files (admin only)")
      .default_member_permissions(Permissions::ADMINISTRATOR)

Handler:
  - Defer the response immediately (scanning may take time).
  - Invoke MediaScanner with config.media_root.
  - Edit deferred response:
      "Scan complete — {newly_registered} new tracks indexed,
       {skipped_existing} already known, {errors} errors."

── /join and /leave remain unchanged ──
── /ping remains unchanged ──

── Final command registration on startup ──

guild_id.set_commands(http, &[
    commands::ping(),
    commands::join(),
    commands::leave(),
    commands::play(),
    commands::scan(),
]).await?;

──────────────────────────────────────────────────────────────
3H. DISPATCH HANDLER ROUTING
──────────────────────────────────────────────────────────────

File: crates/adapters-discord/src/handler.rs

The dispatch() method must route:
  FullEvent::Ready       → log bot name, run startup scan, register commands
  FullEvent::InteractionCreate where interaction is Interaction::Autocomplete(_)
                         → autocomplete_handler(ctx, ac)
  FullEvent::InteractionCreate where interaction is Interaction::Command(_)
                         → command_handler(ctx, cmd)
  _                      → {}

All match arms must include `..` (FullEvent is #[non_exhaustive]).

──────────────────────────────────────────────────────────────
3I. apps/bot STARTUP SEQUENCE
──────────────────────────────────────────────────────────────

main() must execute in this exact order:
  1. Install rustls crypto provider (ring)
  2. Load config from env / dotenvy
  3. Initialize tracing (shared-observability)
  4. Connect to Postgres pool (SQLx)
  5. Run pending migrations (sqlx::migrate!)
  6. Build adapter instances: persistence repos, media store, media scanner
  7. Build application services: EnqueueTrack, JoinVoice, LeaveVoice, RegisterMedia
     Wire Arc<dyn Port> for each required port
  8. Build the Songbird manager
  9. Build the Serenity client with:
       - Token
       - GatewayIntents including GUILD_VOICE_STATES and GUILDS
       - EventHandler (carrying Arc'd application services and search port)
       - .voice_manager(songbird)
  10. Start client

Command registration and startup media scan happen inside FullEvent::Ready
handler, not in main(). Do not block main() on scan.

════════════════════════════════════════════════════════════════════════════════
SECTION 4 — DEPENDENCY ADDITIONS
════════════════════════════════════════════════════════════════════════════════

Add to workspace Cargo.toml [workspace.dependencies] if not present:
  blake3 = "1"
  walkdir = "2"
  symphonia = { version = "0.5", features = ["mp3","aac","isomp4","alac","ogg","vorbis","wav","flac"] }

Add to adapters-media-store/Cargo.toml:
  blake3.workspace = true
  walkdir.workspace = true
  symphonia.workspace = true

No other c