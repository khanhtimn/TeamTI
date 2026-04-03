You are modifying the `teamti` Rust workspace, which uses serenity `next` branch and songbird
`serenity-next` branch. The workspace uses a hexagonal monolith architecture. Do NOT change any
crate boundary or dependency direction unless explicitly instructed.

The full serenity `next` API reference:
  EventHandler: https://github.com/serenity-rs/serenity/blob/next/src/gateway/client/event_handler.rs
  FullEvent enum: https://github.com/serenity-rs/serenity/blob/next/src/model/event.rs
  ClientBuilder: https://github.com/serenity-rs/serenity/blob/next/src/gateway/client/mod.rs
  CreateCommand builder: https://github.com/serenity-rs/serenity/blob/next/src/builder/create_command.rs

BREAKING API RULES (must follow):
- EventHandler uses dispatch(&self, &Context, &FullEvent) only. No separate ready/interaction_create methods.
- FullEvent variants are #[non_exhaustive]. All match arms must include `..`.
- CreateCommand and CreateCommandOption have lifetime parameters: CreateCommand<'static>.
- set_commands takes a slice reference: &[command_fn()].
- Client::builder takes a Token type, not &str.
- voice_manager() not voice_manager_arc().
- rustls crypto provider must be installed first in main().

================================
PROBLEM TO FIX
================================

CURRENT BROKEN BEHAVIOR:
Every call to /play_local path:<string> runs the full import pipeline:
- generates a new UUID
- copies the source file to managed storage as {UUID}_filename.ext
- inserts a new MediaAsset row in Postgres

This means calling /play_local on the same file ten times creates ten copies and ten DB rows.

THIS IS THE BUG. The importer must become idempotent.

================================
WHAT YOU MUST BUILD
================================

1. CONTENT HASH DEDUPLICATION IN IMPORTER
   Location: adapters-media-store/src/importer.rs
   
   Add BLAKE3 content hashing:
   - Before any copy or DB insert, compute the BLAKE3 hash of the source file.
   - Add `content_hash: String` (hex-encoded) to the MediaAsset domain type.
   - Query Postgres for an existing MediaAsset with the same content_hash.
   - If found: return the existing MediaAsset immediately. Do NOT copy, do NOT insert.
   - If not found: proceed with copy, insert with the computed hash.
   
   New behavior:
   - Same file queued 10 times = 1 file on disk, 1 DB row, 10 queue entries pointing to it.
   
   Add crate dependency: blake3 = "1"

2. MEDIA SCANNER
   Location: adapters-media-store/src/scanner.rs
   
   Build a MediaScanner that:
   - Takes a configured media_root directory path from shared-config.
   - Recursively walks the directory using walkdir.
   - For each file with a supported audio extension: mp3, flac, ogg, wav, aac, m4a.
   - Computes its BLAKE3 content hash.
   - Queries Postgres: if content_hash already exists, skip. Log at trace level.
   - If new: extract title and artist from filename heuristic (split on " - ", strip extension).
     If the filename contains " - ", treat left side as artist and right side as title.
     Otherwise treat whole filename (without extension) as title, artist = None.
   - Optionally extract duration using symphonia:
     Open and probe the file with symphonia.
     Read the TimeBase and n_frames from the default track to compute duration_ms.
     If symphonia fails to probe, set duration_ms = None and continue.
   - For local-first imports: the scanner should NOT copy files.
     Instead, create a ManagedBlobRef where the path is the original filesystem path.
     The managed store root is only for physical copies of uploaded content (future).
   - Insert the new MediaAsset into Postgres via the MediaRepository port.
   - Return a ScanReport: total files found, new assets registered, skipped (already known), errors.
   
   The scanner must be callable:
   - At bot startup (before command registration).
   - Via a /scan slash command (guild-only, for the bot admin).
   
   Add crate dependency: blake3 = "1", walkdir = "2"
   Symphonia is already in the workspace.

3. FIX /play_local TO USE CATALOG LOOKUP
   Location: adapters-discord/src/commands.rs and application/src/services/enqueue_track.rs
   
   Current broken flow:
   /play_local path:<string> → importer → copy file → insert DB → enqueue
   
   New correct flow:
   /play_local path_or_query:<string> → search MediaAsset by title/filename ILIKE → enqueue by asset_id
   
   The command option name should change from `path` to `query`.
   The user should never type raw filesystem paths. They type a search term.
   The command should use Discord autocomplete.
   
   Details:
   - When the user types in the /play_local query: option, Discord sends an autocomplete
     interaction.
   - The handler queries Postgres: SELECT id, title, original_filename FROM media_assets
     WHERE title ILIKE '%{query}%' OR original_filename ILIKE '%{query}%' LIMIT 25;
   - Return up to 25 choices as (display_name: title or filename, value: asset_id as string).
   - When the user confirms the selection, the interaction comes in as a Command interaction
     with the chosen asset_id string as the option value.
   - The enqueue_track service resolves the asset by ID, gets the blob path, and enqueues.
   
   HOW TO HANDLE AUTOCOMPLETE IN SERENITY NEXT:
   In the dispatch() handler, match FullEvent::InteractionCreate { interaction, .. }.
   Check if it is Interaction::Autocomplete(autocomplete_interaction).
   Read the focused option value from autocomplete_interaction.data.
   Run the Postgres ILIKE query.
   Respond with autocomplete_interaction.create_response(http, CreateAutocompleteResponse::new()
     .set_choices(vec![...])).await.
   Check the serenity next source for exact AutocompleteInteraction and
   CreateAutocompleteResponse types.
   
   IMPORTANT: CreateCommand must mark the query option as autocomplete:
   CreateCommandOption::new(CommandOptionType::String, "query", "Track name to search")
       .required(true)
       .set_autocomplete(true)

4. POSTGRES MIGRATION UPDATE
   Location: migrations/
   
   Add a new migration (do NOT modify the existing one):
   File: migrations/0002_add_content_hash.sql
   
   - Add column: ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS content_hash TEXT;
   - Create unique index: CREATE UNIQUE INDEX IF NOT EXISTS idx_media_assets_content_hash
     ON media_assets(content_hash) WHERE content_hash IS NOT NULL;
   - Add GIN index for future full text search (add now, use later):
     ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS search_vector tsvector
       GENERATED ALWAYS AS (
         to_tsvector('english', coalesce(title, '') || ' ' || coalesce(original_filename, ''))
       ) STORED;
     CREATE INDEX IF NOT EXISTS idx_media_assets_search ON media_assets USING GIN(search_vector);
   
   Run migrations on startup as before.

5. ADD /scan COMMAND
   Location: adapters-discord/src/commands.rs
   
   Register a /scan guild slash command.
   Handler:
   - Defer the interaction response immediately (scanning may take a moment).
   - Call the media scanner with the configured media_root.
   - Edit the deferred response with the ScanReport: "Scanned X files. Y new, Z skipped, W errors."
   
   The /scan command should only succeed if invoked by a guild administrator.
   Check the member permissions in the CommandInteraction before proceeding.

================================
CRATE DEPENDENCY ADDITIONS
================================

Add to adapters-media-store/Cargo.toml:
- blake3 = "1"
- walkdir = "2" (already in workspace, ensure it is listed)

Symphonia is already in the workspace. Import it in adapters-media-store.

No other crate boundary changes. Do NOT add blake3 or walkdir to domain or application.
Hashing and scanning are infrastructure concerns.

================================
DOMAIN MODEL CHANGE
================================

In crates/domain/src/media.rs:

Add to MediaAsset:
  pub content_hash: Option<String>,

In adapters-persistence:
Update MediaRepository::find_by_content_hash(hash: &str) -> Result<Option<MediaAsset>>
Use sqlx query: SELECT ... FROM media_assets WHERE content_hash = $1 LIMIT 1

================================
CONFIGURATION ADDITIONS
================================

In crates/shared-config/src/lib.rs, add:
  pub media_root: PathBuf,  // path to local managed media root directory

In .env.example add:
  MEDIA_ROOT=/path/to/your/music

================================
WHAT NOT TO CHANGE
================================

- Do not change the crate dependency graph or workspace structure.
- Do not modify migration 0001. Add 0002.
- Do not change the Songbird voice/playback logic.
- Do not change how tokens, intents, or the Serenity client are constructed.
- Do not add an event bus.
- Do not add web/portal logic.
- Do not change how ManagedBlobRef is used in the voice adapter — only how it is
  created in the importer and scanner.

================================
QUALITY BAR
================================

- cargo check --workspace must pass after your changes.
- sqlx compile-time macros must match the updated schema including content_hash column.
  Run sqlx prepare if needed or ensure DATABASE_URL is set for the check.
- The scanner must be callable both at startup and from the /scan command handler.
- The importer must be idempotent: identical input file = identical output asset.
- /play_local must not accept raw filesystem paths from the user.
- Autocomplete must query Postgres, not an in-memory list.
- Comments should be brief and meaningful.
- Do not produce placeholder TODO-only implementations.
  The scan and autocomplete flows must be real, not stubbed.

================================
OUTPUT ORDER
================================

1. Summarize what changed and why.
2. Show the updated domain model.
3. Show the new migration file.
4. Show the updated importer (idempotent, with BLAKE3).
5. Show the new scanner.
6. Show the updated enqueue_track service.
7. Show the updated commands.rs (autocomplete + /scan).
8. Show the updated handler.rs dispatch() for autocomplete handling.
9. Show config additions.
10. Show updated .env.example.
11. State any API assumptions about serenity next types
    (AutocompleteInteraction, CreateAutocompleteResponse) with links to source.