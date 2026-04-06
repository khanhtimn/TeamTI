# TeamTI v3 — Pass 3 Design Spec
## User Layer: Playlists, Favourites, Listen History & Radio

> All design decisions are locked. This document is the authoritative
> reference for Pass 3 implementation. Attach it alongside the current
> migration files and schema when sending to the agent.

---

## Locked Decisions

| Topic | Decision |
|-------|----------|
| Identity | Global Discord user snowflake (`user_id TEXT`) — consistent across all servers |
| Favourites scope | Global to user — no guild scoping |
| Playlist scope | Global to user — visibility is a global flag, not per-server |
| Listen events | Guild-stamped — `guild_id` retained for context |
| Completion signal | Threshold-based (see §Listen Events) + multi-factor scoring at query time |
| Collaborative playlists | Binary: `owner` vs `collaborator`; only owner can invite; removed collaborator's tracks stay |
| Radio seed | Current playing track → genre/artist/affinity expansion |
| Radio refill | Fully silent — no user-facing notification |
| `/play` empty query | Mix: recent history + favourites + recommendations |
| Cold start | Globally most-played across all guilds; fallback random if no data |
| Web portal | Deferred to v4 — Discord-first only |
| Recommendation scope | If engine grows too complex mid-pass, defer advanced scoring; random-from-library is an acceptable Pass 3 floor |

---

## Schema Changes

### Migration file to modify: `0003_user_library-3.sql`

Make all changes inline. No new migration file.

After editing, reset and re-apply:
```bash
cargo sqlx database drop && cargo sqlx database create
cargo sqlx migrate run
cargo sqlx prepare --workspace
```

---

#### 1. `playlists` — add visibility, description, updated_at

```sql
CREATE TABLE IF NOT EXISTS playlists (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    owner_id    TEXT NOT NULL,
    -- 'private': only owner + collaborators can see
    -- 'public':  visible to all users who share a guild with the owner
    visibility  TEXT NOT NULL DEFAULT 'private'
                    CHECK (visibility IN ('private', 'public')),
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

---

#### 2. `playlist_items` — add added_by, drop problematic UNIQUE on position

The existing `UNIQUE (playlist_id, position)` constraint makes reordering
require shifting all intermediate positions inside a transaction and
fighting the constraint. Drop it. Position is now a soft ordering hint,
not a uniqueness key. Ties are resolved by `added_at ASC`.

```sql
CREATE TABLE IF NOT EXISTS playlist_items (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    playlist_id UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL DEFAULT 0,
    added_by    TEXT NOT NULL,   -- Discord user ID; owner or collaborator
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now()
    -- No UNIQUE on position — allows duplicate positions during reorder,
    -- resolved by added_at. Tracks are always ordered: position ASC, added_at ASC.
    -- Duplicate tracks in a playlist are intentionally allowed (Q7).
);
```

---

#### 3. `playlist_collaborators` — new table

```sql
CREATE TABLE IF NOT EXISTS playlist_collaborators (
    playlist_id UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL,
    added_by    TEXT NOT NULL,   -- must be the playlist owner_id (enforced in app layer)
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (playlist_id, user_id)
);
```

---

#### 4. `listen_events` — add play_duration_ms

```sql
CREATE TABLE IF NOT EXISTS listen_events (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         TEXT NOT NULL,
    track_id        UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    guild_id        TEXT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- NULL until the event is closed (track ends, skipped, or bot leaves vc)
    -- Set to elapsed playback time, not wall time.
    play_duration_ms INTEGER,
    -- Computed at event close: play_duration_ms / tracks.duration_ms >= 0.8
    -- (The 0.8 threshold is a named constant in the application layer.)
    completed       BOOLEAN NOT NULL DEFAULT false
);
```

The completion threshold constant lives in the application layer, not
in the database. Pass 3 defines it as:

```rust
// crates/application/src/lib.rs or a constants module:
/// A listen event is "completed" when the user listened to at least
/// this fraction of the track duration.
pub const LISTEN_COMPLETION_THRESHOLD: f32 = 0.80;
```

---

#### 5. `listen_events` index — add guild context index

Add to `0004_indexes-4.sql` inline (no new migration):

```sql
-- Efficient recent-history lookup per user
CREATE INDEX IF NOT EXISTS idx_listen_events_user_recent
ON listen_events(user_id, started_at DESC);

-- Efficient per-track global play count for cold-start recommendations
CREATE INDEX IF NOT EXISTS idx_listen_events_track_global
ON listen_events(track_id)
WHERE completed = true;
```

---

#### 6. `favourites` — no changes needed

The existing schema is correct: global to user, no guild_id.
Add an index for efficient lookup if not already present:

```sql
-- Add to 0004_indexes-4.sql:
CREATE INDEX IF NOT EXISTS idx_favorites_track
ON favorites(track_id);
```

---

## Application Layer — New Ports

Add to `crates/application/src/ports/`.

### `playlist.rs`

```rust
#[async_trait]
pub trait PlaylistPort: Send + Sync {
    // ── Playlist CRUD ────────────────────────────────────────────
    async fn create_playlist(
        &self, owner_id: &str, name: &str, description: Option<&str>,
    ) -> Result<Playlist, AppError>;

    async fn rename_playlist(
        &self, playlist_id: Uuid, owner_id: &str, new_name: &str,
    ) -> Result<(), AppError>;

    async fn delete_playlist(
        &self, playlist_id: Uuid, owner_id: &str,
    ) -> Result<(), AppError>;

    async fn set_visibility(
        &self, playlist_id: Uuid, owner_id: &str, visibility: PlaylistVisibility,
    ) -> Result<(), AppError>;

    // ── Items ────────────────────────────────────────────────────
    async fn add_track(
        &self, playlist_id: Uuid, track_id: Uuid, added_by: &str,
    ) -> Result<PlaylistItem, AppError>;

    async fn remove_track(
        &self, playlist_id: Uuid, item_id: Uuid, requesting_user: &str,
    ) -> Result<(), AppError>;

    async fn reorder_track(
        &self, playlist_id: Uuid, item_id: Uuid,
        new_position: i32, requesting_user: &str,
    ) -> Result<(), AppError>;

    // ── Queries ──────────────────────────────────────────────────
    async fn list_user_playlists(
        &self, owner_id: &str,
    ) -> Result<Vec<PlaylistSummary>, AppError>;

    async fn get_playlist_items(
        &self, playlist_id: Uuid, requesting_user: &str, page: i64, page_size: i64,
    ) -> Result<PlaylistPage, AppError>;

    async fn get_playlist_tracks(
        &self, playlist_id: Uuid, requesting_user: &str,
    ) -> Result<Vec<TrackSummary>, AppError>;

    // ── Collaboration ────────────────────────────────────────────
    async fn add_collaborator(
        &self, playlist_id: Uuid, owner_id: &str, new_collaborator_id: &str,
    ) -> Result<(), AppError>;

    async fn remove_collaborator(
        &self, playlist_id: Uuid, owner_id: &str, collaborator_id: &str,
    ) -> Result<(), AppError>;

    async fn list_collaborators(
        &self, playlist_id: Uuid, requesting_user: &str,
    ) -> Result<Vec<String>, AppError>;   // returns user IDs
}
```

### `user_library.rs`

```rust
#[async_trait]
pub trait UserLibraryPort: Send + Sync {
    // ── Favourites ───────────────────────────────────────────────
    async fn add_favourite(
        &self, user_id: &str, track_id: Uuid,
    ) -> Result<(), AppError>;

    async fn remove_favourite(
        &self, user_id: &str, track_id: Uuid,
    ) -> Result<(), AppError>;

    async fn is_favourite(
        &self, user_id: &str, track_id: Uuid,
    ) -> Result<bool, AppError>;

    async fn list_favourites(
        &self, user_id: &str, page: i64, page_size: i64,
    ) -> Result<FavouritesPage, AppError>;

    // ── Listen history ───────────────────────────────────────────
    async fn open_listen_event(
        &self, user_id: &str, track_id: Uuid, guild_id: &str,
    ) -> Result<Uuid, AppError>;   // returns listen_event id

    async fn close_listen_event(
        &self, event_id: Uuid, play_duration_ms: i32,
        track_duration_ms: i32,
    ) -> Result<(), AppError>;
    // Internally computes completed = play_duration_ms / track_duration_ms >= THRESHOLD

    async fn recent_history(
        &self, user_id: &str, limit: i64,
    ) -> Result<Vec<TrackSummary>, AppError>;
}
```

### `recommendation.rs`

```rust
#[async_trait]
pub trait RecommendationPort: Send + Sync {
    /// Generate a ranked list of recommended tracks for a user.
    /// Used for radio refill and /play empty-query suggestions.
    ///
    /// seed_track_id: current playing track (radio context).
    ///   If None, derive seed purely from user profile.
    /// exclude: track IDs already in the queue (do not repeat).
    /// limit: how many tracks to return.
    async fn recommend(
        &self,
        user_id:       &str,
        seed_track_id: Option<Uuid>,
        exclude:       &[Uuid],
        limit:         usize,
    ) -> Result<Vec<TrackSummary>, AppError>;
}
```

---

## Domain Types (new)

Add to `crates/domain/src/`.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistVisibility {
    Private,
    Public,
}

impl PlaylistVisibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public  => "public",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id:          Uuid,
    pub name:        String,
    pub owner_id:    String,
    pub visibility:  PlaylistVisibility,
    pub description: Option<String>,
    pub created_at:  chrono::DateTime<chrono::Utc>,
    pub updated_at:  chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct PlaylistSummary {
    pub id:         Uuid,
    pub name:       String,
    pub owner_id:   String,
    pub visibility: PlaylistVisibility,
    pub track_count: i64,
}

#[derive(Debug, Clone)]
pub struct PlaylistItem {
    pub id:          Uuid,
    pub playlist_id: Uuid,
    pub track_id:    Uuid,
    pub position:    i32,
    pub added_by:    String,
    pub added_at:    chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct PlaylistPage {
    pub items:      Vec<(PlaylistItem, TrackSummary)>,
    pub total:      i64,
    pub page:       i64,
    pub page_size:  i64,
}

#[derive(Debug, Clone)]
pub struct FavouritesPage {
    pub tracks:    Vec<TrackSummary>,
    pub total:     i64,
    pub page:      i64,
    pub page_size: i64,
}
```

---

## AppError — New Variants

```rust
// In AppError enum:
#[error("playlist error ({kind}): {detail}")]
Playlist { kind: PlaylistErrorKind, detail: String },

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlaylistErrorKind {
    #[error("not found")]           NotFound,
    #[error("permission denied")]   Forbidden,
    #[error("already exists")]      AlreadyExists,
    #[error("collaborator limit")]  CollaboratorLimit,
}
```

---

## Access Control Rules

Enforced in the application layer (port implementations), not in SQL.

| Operation | Allowed |
|-----------|---------|
| View playlist items | Owner, collaborators, any user if `visibility = 'public'` |
| Add/remove tracks | Owner + collaborators |
| Rename/delete playlist | Owner only |
| Set visibility | Owner only |
| Add collaborator | Owner only |
| Remove collaborator | Owner only |
| Reorder tracks | Owner + collaborators |

If a non-owner, non-collaborator attempts a write operation on a private
playlist, return `AppError::Playlist { kind: PlaylistErrorKind::Forbidden }`.
If the playlist does not exist or is private and inaccessible, return
`NotFound` (not `Forbidden`) to avoid leaking existence.

---

## Recommendation Engine — Pass 3 Floor

Keep the recommendation implementation simple. The floor for Pass 3 is:

```
score(track) =
    W_genre   × genre_overlap(track, user_genre_affinity)
  + W_artist  × artist_affinity(track, user_top_artists)
  + W_popular × global_play_count(track)       ← cold start signal
  + W_fav     × is_favourited_similar(track)   ← 0 or 1 × constant
```

Where:
- `user_genre_affinity`: top N genres from completed listen events + favourites
- `user_top_artists`: top N artists by completed listen count
- `global_play_count`: normalised count of completed events across all guilds
- `W_*` constants defined in the application layer (not DB)

If computing this at query time is too slow (>200ms) for a cold Discord
autocomplete, fall back to:
1. Random sample of 25 tracks the user hasn't heard recently
2. Top 25 globally most-played tracks
Document the fallback clearly with a TODO for a later scoring pass.

**Do not** build a separate recommendation worker or background job in
Pass 3. Compute on-demand. A materialized affinity cache can be added
in Pass 3.1 if query latency requires it.

---

## Discord Commands — New Surface

### `/playlist` group

```
/playlist create  name:<text> [description:<text>]
/playlist delete  name:<autocomplete — your playlists>
/playlist rename  name:<autocomplete> new_name:<text>
/playlist add     playlist:<autocomplete> track:<autocomplete — library search>
/playlist remove  playlist:<autocomplete> track:<autocomplete — items in playlist>
/playlist play    name:<autocomplete>
/playlist list    [user:<mention>]   ← lists public playlists if other user
/playlist view    name:<autocomplete>  ← paginated embed with buttons
/playlist share   name:<autocomplete>  ← toggle public/private
/playlist invite  name:<autocomplete> user:<mention>
/playlist kick    name:<autocomplete> user:<mention>
```

### `/favourite` group

```
/favourite add    [track:<autocomplete>]  ← defaults to currently playing
/favourite remove [track:<autocomplete>]
/favourite list
```

### `/radio`

```
/radio  ← starts radio mode seeded from current track.
          If nothing is playing, seeds from user taste profile.
          Silently refills the queue when ≤2 tracks remain.
```

### `/play` empty query

When `/play` is invoked with no query string, autocomplete options
return a mix sourced from:
1. Last 8 distinct tracks from `listen_events` for this user
2. Up to 8 tracks from `favourites` not already in recent history
3. Up to 9 recommended tracks filling the remaining 25-option budget

Order: recents first, then favourites, then recommendations.

### `/history`

```
/history  ← paginated embed of recent listen events for the invoking user
```

---

## Pagination Pattern (Discord Components)

All paginated views (`/playlist view`, `/favourite list`, `/history`)
use the same component pattern:

- Initial response: embed + row of buttons `[◀ Prev] [Page 1/N] [Next ▶]`
- Page size: 10 items per embed page
- Buttons are disabled when at boundary (first/last page)
- State is encoded in the custom_id of each button:
  `"playlist_page:{playlist_id}:{page_number}:{requesting_user_id}"`
  The `requesting_user_id` prevents other users from navigating
  someone else's pagination session.
- Interaction timeout: 5 minutes (Discord default component timeout)
- After timeout, edit the message to remove buttons and add
  "Session expired — run the command again."

---

## Radio Refill Logic

```
trigger: queue length drops to ≤ RADIO_REFILL_THRESHOLD (default: 2)
action:
  1. Identify the last track that played (seed)
  2. Call RecommendationPort::recommend(
         user_id       = invoking_user,
         seed_track_id = last_played_id,
         exclude       = current_queue_track_ids,
         limit         = RADIO_BATCH_SIZE (default: 5)
     )
  3. Append results to queue
  4. Emit no message — fully silent
```

Radio mode is a boolean flag on the active player session. The `/radio`
command sets it; the `/stop` command or the player finishing without
radio mode unsets it.

---

## Crates Affected

| Crate | Change |
|-------|--------|
| `crates/domain` | New types: Playlist, PlaylistItem, PlaylistSummary, PlaylistVisibility, FavouritesPage, PlaylistPage |
| `crates/application` | New ports: PlaylistPort, UserLibraryPort, RecommendationPort; new AppError variants; LISTEN_COMPLETION_THRESHOLD constant |
| `crates/adapters-persistence` | Implement all three ports against PostgreSQL |
| `apps/bot` | New slash command groups: /playlist, /favourite, /radio; update /play empty-query handler; pagination component handler |

No new crates required for Pass 3.

---

## Verification Plan

```bash
# Schema + queries compile
cargo sqlx migrate run
cargo sqlx prepare --workspace
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# Manual — playlist lifecycle
# Create a playlist, add 3 tracks, view paginated, reorder, play it

# Manual — collaborative playlist
# Owner creates playlist, invites collaborator
# Collaborator adds a track
# Owner removes collaborator — track must remain

# Manual — radio mode
# /play a track, /radio
# Let the queue drop to ≤2 tracks
# Verify queue refills silently with ≥1 new track

# Manual — /play empty query
# A user with listen history: autocomplete should show recent tracks
# A user with no history: autocomplete should show globally popular tracks
```
