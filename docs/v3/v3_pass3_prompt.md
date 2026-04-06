# TeamTI v3 — Pass 3 Implementation Prompt
## User Layer: Playlists, Favourites, Listen History & Radio

> Attach alongside: `teamti_v3_pass3_design.md` (schema, ports, domain
> types, access control rules, and all locked decisions).
> Also attach: current migration files, `main.rs`, and the existing
> slash command implementations so the agent understands the established
> patterns before writing new code.
>
> The design spec is the authoritative source for all type names,
> SQL, port signatures, and access control logic. This prompt describes
> the goals, UX expectations, and implementation philosophy. Read both
> before writing a single line.

---

## What This Pass Builds

TeamTI is a Discord music bot backed by a local media library. Every
user in every server the bot is in has a **globally consistent identity**
— their Discord user ID. Pass 3 gives each user their own persistent
layer on top of the shared library: playlists they curate, tracks they
favourite, a history of what they have listened to, and a radio mode
that learns their taste over time.

This is the foundation of the platform's personal experience. Every
feature built here will compound — listen history feeds recommendations,
recommendations feed radio, favourites weight radio seeds, and
playlists become the user's primary curation surface. Build it cleanly.

---

## End Goals — What Must Work When This Pass Is Done

### Playlists

A user can type `/playlist create My Mix` and immediately have a
private, empty playlist. From there, every sane playlist operation
works via slash commands:

- They can add any track from the library to the playlist. The track
  search uses the existing Tantivy autocomplete — no new search
  infrastructure is needed.
- They can remove a track, reorder tracks, rename the playlist, and
  delete it.
- They can play the entire playlist with `/playlist play` — all tracks
  are queued in position order.
- They can browse the playlist with `/playlist view`, which shows a
  paginated embed (10 tracks per page) with ◀ / ▶ buttons. The buttons
  work for the invoking user only — another user clicking them should
  get a quiet ephemeral "this isn't your session" response.
- They can make the playlist public with `/playlist share`, making it
  browsable and playable by anyone who shares a server with them.
- They can invite a collaborator with `/playlist invite @user`. The
  collaborator can add and remove tracks. Only the owner can invite,
  kick collaborators, rename, delete, or change visibility.
- A track added to a playlist by a collaborator stays in the playlist
  even after the collaborator is removed.
- The same track can appear in a playlist more than once.

**Autocomplete:** `/playlist` subcommands that take a playlist name
autocomplete against the invoking user's own playlists (for write
operations) or all accessible playlists (for read operations like play
and view — includes public playlists from other users).

### Favourites

A user can favourite the currently playing track with `/favourite add`
(no arguments needed — defaults to current track). They can also search
for a track to favourite. `/favourite list` shows a paginated embed of
their favourited tracks. `/favourite remove` takes an autocompleted
list of their current favourites.

Favouriting a track is an explicit taste signal — it is used by the
recommendation engine. There is no public leaderboard of who favourited
what, but favourite counts may be used internally.

### Listen History

Every track play is automatically recorded — the user does not take
any action. When a user invokes `/play` with no search query, the
autocomplete options show a blend of their recent plays, their
favourites, and recommended tracks (up to Discord's 25-option limit).
This is the primary discovery surface for returning users.

`/history` shows the user's own recent listen history as a paginated
embed. This is private — only visible to the invoking user (ephemeral
response, or at least clearly labelled as personal).

A listen event is opened when a track starts playing for a user and
closed when it ends (track finishes, is skipped, or the bot leaves VC).
The raw play duration in milliseconds is stored. Completion is computed
from that duration against the track's total duration at close time.

### Radio

`/radio` starts radio mode seeded from the currently playing track.
If nothing is playing, it seeds from the user's taste profile. From
that point, the queue is managed automatically — when it drops to two
or fewer tracks remaining, the system silently adds a small batch of
recommendations. The user never sees a "radio added tracks" message.
Radio mode ends when `/stop` is called or playback ends.

The recommendation picks tracks based on genre overlap with the seed,
artist affinity from listen history, globally most-played tracks (cold
start signal), and favourites similarity. All of this is computed at
query time from existing tables — no background jobs, no new workers.
If the computation is too slow, fall back to globally most-played then
random — document this clearly with a TODO.

---

## Implementation Philosophy

### Work in layers, bottom to top

1. Schema changes first (inline in existing migration files)
2. Domain types in `crates/domain`
3. Port trait definitions in `crates/application/src/ports/`
4. Port implementations in `crates/adapters-persistence`
5. Discord command handlers in `apps/bot`
6. Wire everything in `main.rs`

Do not write Discord handlers before the port implementations exist.
Do not write port implementations before the domain types compile.
Each layer must compile cleanly before the next is started.

### Access control lives in the application layer

SQL does not enforce playlist ownership or visibilitye port
implementations in `adapters-persistence` check ownership and
collaborator status before executing writes. The rule is:

- Private playlist + non-owner + non-collaborator = `AppError::Playlist { kind: NotFound }`
  (not `Forbidden` — do not leak existence of private playlists)
- Write operations (add/remove/reorder) from a non-owner, non-collaborator
  on any playlist = `AppError::Playlist { kind: Forbidden }`
- Collaborator invite by non-owner = `Forbidden`

### Pagination is a shared pattern

All three paginated views (playlist browse, favourites list, history)
use the same component structure: embed + ◀/▶ buttons, page state
encoded in the button `custom_id`, 5-minute session timeout after which
buttons are removed and a "run the command again" note is added.

Build this as a reusable helper — do not copy-paste three separate
pagination implementations. The custom_id format is:
`"{view_type}:{resource_id}:{page}:{user_id}"`
where `user_id` gates which user can interact with the buttons.

### Error messages are user-facing

Every `AppError::Playlist` variant must have a corresponding
user-friendly Discord message. Examples:

| Error | Discord message |
|-------|----------------|
| `NotFound` | "Playlist not found." |
| `Forbidden` | "You don't have permission to do that." |
| `AlreadyExists` | "You already have a playlist with that name." |
| `CollaboratorLimit` | "This playlist has reached the collaborator limit." |

Do not expose internal error detail strings to Discord users.
Internal details go to `tracing::warn!` / `tracing::error!`.

### Listen events open and close correctly

Every track play opens a listen event for every user currently in the
voice channel. Every track end closes all open events for that track
in that guild. This means when a track is skipped or the bot is
disconnected, all open events for that track must be closed with the
elapsed duration. Do not leave dangling open events.

A good place to close events: wherever the existing player code
currently transitions between tracks or handles disconnect.

### Tantivy reindex on no new infrastructure

The `/playlist add` command takes a track argument via autocomplete.
This autocomplete already uses Tantivy search from Pass 1. No changes
to the search layer are needed — just pass the resolved `track_id`
from the autocomplete result through to `PlaylistPort::add_track`.

---

## What This Pass Does NOT Do

- No web portal, no HTTP endpoints — Discord only.
- No background recommendation workers — compute on-demand.
- No cross-server playlist sharing — playlists are global to the user,
  but "public" means visible to users in shared servers, not to
  the entire internet.
- No social features (following users, shared feeds, public profiles).
- No listen count display on track embeds (deferred).
- No playlist cover art.
- No import/export of playlists.
- No advanced recommendation algorithms — the scoring function in the
  design spec is the ceiling for Pass 3, not the floor.

---

## Definition of Done

Pass 3 is complete when:

1. `cargo sqlx migrate run` applies cleanly on a fresh database.
2. `cargo sqlx prepare --workspace` passes with zero errors.
3. `cargo build --workspace` produces zero errors and zero warnings.
4. `cargo test --workspace` passes.
5. All seven slash command groups work end-to-end in a real Discord
   server:
   - `/playlist create` → `/playlist add` → `/playlist view` (paginated)
     → `/playlist play` → `/playlist share` → `/playlist invite`
   - `/favourite add` (from currently playing) → `/favourite list`
   - `/radio` (with a track playing) → queue refills silently
   - `/play` with no query → 25 autocomplete options including
     recent history, favourites, and recommendations
   - `/history` → paginated ephemeral embed
6. A collaborator can add a track to a joint playlist, be removed by
   the owner, and the track remains.
7. A private playlist is not visible or accessible to non-owner,
   non-collaborator users.
8. Listen events are closed correctly on skip and on bot disconnect.
