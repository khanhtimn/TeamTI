# TeamTI v4 — Audit Pass 2.3
## UX, Interaction, and Feedback Audit for `/play` Search

> **Scope.** This pass focuses less on raw correctness and more on the
> *experience* of using `/play` search and autocomplete in Discord.
> Agents should still fix correctness bugs they encounter, but the primary
> mission is to evaluate the UX end-to-end:
>
> 1. How does the user understand what `/play` is doing while typing?
> 2. Are autocomplete suggestions informative, compact, and predictable?
> 3. Does selecting a result feel reliable and low-friction?
> 4. Are failure cases and fallbacks understandable without being noisy?
> 5. Does the interaction model scale well to local tracks + cached YouTube
>    tracks + transient YouTube search results?
>
> Agents are encouraged to make suggestions, propose alternatives, and apply
> UX-focused refinements where they materially improve clarity, confidence,
> or speed — while preserving the locked architecture from Pass 2.
>
> **External constraints:**
> - Discord autocomplete supports only up to 25 choices. [web:666][web:668][web:673]
> - Autocomplete and choices are constrained enough that dense, well-trimmed
>   strings matter more than exhaustive results. [web:651][web:655][web:670]
>
> Start with:
> ```bash
> cargo build --workspace 2>&1 | grep -E "^error|^warning"
> cargo test --workspace
> cargo sqlx prepare --workspace
> ```

---

## UX Questions to Answer

The audit should explicitly answer these before concluding:

1. **Can a first-time user infer how `/play` behaves from the UI alone?**
   Especially for `yt:` prefix, pasted URLs, and mixed local/YouTube results.

2. **Are autocomplete rows carrying the right amount of information?**
   Enough to choose confidently, but not so much that rows become noisy,
   repetitive, or truncated into uselessness.

3. **Does the experience create trust?**
   Users should feel that selecting a row will play *that* thing, not a
   vaguely similar guess. Search should feel deliberate, not magical.

4. **Is the interaction model discoverable?**
   If `yt:` is the only explicit YouTube-search affordance, can users learn it
   naturally, or should the UI hint at it more clearly?

5. **Do the feedback surfaces form one coherent language?**
   Autocomplete text, `/play` response messages, queue entries, and now playing
   should feel like one system, not four unrelated representations.

---

## Findings Index

| ID | Severity | Area | Title |
|----|----------|------|-------|
| U1 | UX-Critical | Autocomplete semantics | Result rows are hard to distinguish or interpret quickly |
| U2 | UX-Critical | Selection confidence | Users cannot reliably tell playable tracks from transient search stubs |
| U3 | UX-Critical | Interaction discoverability | `yt:` mode is too hidden; users have no clear path to intentional YouTube-only search |
| U4 | UX-Critical | Submission feedback | `/play` responses do not confirm enough context after selection |
| U5 | UX-Critical | Error feedback | Unsupported URLs, empty results, and no-match states feel confusing or silent |
| M1 | Major | Information density | Rows waste limited 25-choice/100-char budget on low-value text |
| M2 | Major | Formatting consistency | Source icons, artist fallbacks, durations, and title truncation are inconsistent across surfaces |
| M3 | Major | Ranking UX | The "best" result is technically correct but not obviously the most likely user intent |
| M4 | Major | Preview UX | URL preview is too weak, too raw, or not confidence-building enough |
| M5 | Major | Mixed-source UX | Local, cached YouTube, and YouTube-search results feel visually too similar or too fragmented |
| M6 | Major | Empty / loading states | The repeated-query model is invisible; users may not understand why YouTube results appear later |
| O1 | Optimization | Interaction language | Copy can be tightened and standardized to reduce cognitive load |
| O2 | Optimization | Display strategy | Smarter truncation or field ordering could improve scannability |
| O3 | Optimization | Discoverability | Small hints or contextual affordances could teach `yt:` without adding commands |
| O4 | Optimization | Fallback messaging | Better follow-up messages can increase trust after auto-queued YouTube searches |
| S1 | Explore | UX copy system | Unify string patterns used in autocomplete, submit responses, queue, and NP |
| S2 | Explore | Choice formatting | Evaluate alternate row layouts under Discord constraints |
| S3 | Explore | Ranking presentation | Consider whether visual grouping or slight source-priority tweaks improve choice confidence |
| S4 | Explore | New-user experience | Assess how a user with zero prior knowledge would discover the search model |

---

## Core UX Audits

### U1 — Autocomplete rows must be instantly legible

**Problem to look for.** Discord autocomplete is a tiny, high-pressure UI.
There are only 25 slots, and users decide in a glance. [web:666][web:668]
A row that technically contains the right info can still be bad UX if:
- the icon is too subtle to notice
- title and artist are visually unbalanced
- duration crowds out useful identity cues
- truncation hides the differentiating part of the title
- all rows look too similar when scanning quickly

**Audit tasks:**
- Observe real autocomplete output for local tracks, downloaded YouTube tracks,
  and `youtube_search` stubs
- Judge scannability, not just correctness
- Compare the current format against at least 2 alternatives, e.g.:
  - `🎵 Title — Artist · 3:33`
  - `🎵 Title · Artist · 3:33`
  - `🎵 Title — Artist` (omit duration for longer names)

**Allowed improvements:**
- rebalance field order
- omit duration when it materially harms differentiation
- apply smarter truncation that preserves the title more than the suffix

If changed, explain why the new layout improves fast selection under Discord's
narrow constraints. [web:651][web:670]

---

### U2 — Users must understand what is "real" vs "search result"

**Problem.** A downloaded YouTube track and a transient `youtube_search`
result both use the 📺 icon by current design. This is architecturally clean,
but UX-wise it may be ambiguous:
- one is already known to the bot and likely fast/playable
- the other is just a search preview that may still need Pass 1 work

If users cannot tell the difference, they may not understand why one selection
feels instant and another takes a few seconds.

**Audit tasks:**
- Evaluate whether a single 📺 icon is sufficient
- Test whether users need a stronger cue, such as:
  - `📺` for cached/downloaded YouTube track
  - `🔎` or `📺?` for YouTube search stub
  - suffix labels like `· YouTube` vs `· Search`

**Constraint:** keep UX compact. The answer may still be "keep one icon" if it
proves cleaner overall, but the agent must justify it from a UX perspective.

---

### U3 — `yt:` discoverability

**Problem.** `yt:` is powerful but hidden. A first-time user may never discover
it organically.

**Audit tasks:**
Explore lightweight ways to teach `yt:` without violating the locked command
surface. Suggestions may include:
- a special hint row when local results are sparse, e.g.
  `🔍 Tip: use yt:query for YouTube-only search`
- showing a contextual hint only when no local results are found
- embedding the hint into empty-state or failure responses

Be conservative. Hints should not crowd out useful results or spam power users.
If you implement a hint, gate it carefully (for example only when local_count=0
and query length ≥ 3).

---

### U4 — Submission feedback must confirm intent

**Problem.** After choosing an autocomplete result, the slash command response
is one of the few explicit feedback surfaces available. If the response says
only `Added to queue: {title}`, that may be too weak when there are multiple
versions of the same song.

**Audit tasks:**
Check whether `/play` follow-up messages should include slightly richer context:
- `Added to queue: Never Gonna Give You Up — Rick Astley`
- if source matters: `Added to queue: Never Gonna Give You Up — Rick Astley`
  (keep source hidden if that's the desired product feel)

Also audit consistency with queue entries and now-playing embeds. The naming of
a track should feel the same across surfaces.

Allowed improvement: modestly enrich the response copy if it materially
improves confidence and does not add clutter.

---

### U5 — Empty, no-match, and unsupported-input states must not feel broken

**Problem.** The current design intentionally returns empty autocomplete for
unsupported non-YouTube URLs and delays YouTube results via repeated-query
stability. That is clean technically, but the UX risk is silence:
- unsupported URL → nothing visible, user may think autocomplete is broken
- first invocation of rare query → only local nothingness, user may think there
  are no results at all
- second/third invocation → YouTube rows suddenly appear without explanation

**Audit tasks:**
Evaluate whether the current silence is acceptable or whether subtle guidance is
needed. Consider suggestions such as:
- no autocomplete hint for unsupported URLs (keep silent) but better submit-time
  error copy
- if no local results and query length sufficient, a low-noise hint row:
  `🔍 Searching YouTube if you keep typing…`
- or for empty standard mode: `🔍 No local matches yet`

Agents may recommend against hints if they create clutter. The goal is not more
text; the goal is reduced confusion.

---

## Major UX Audits

### M1 — Use the 25-choice budget wisely

Discord only displays 25 autocomplete choices. [web:666][web:673] Every row is
precious. Audit whether the current merge heuristic wastes rows on low-value
entries or overexposes similar near-duplicates.

Explore:
- whether near-identical variants should be collapsed or de-prioritized
- whether title-only duplicates from multiple sources should be spaced apart
- whether showing 20 YouTube results when only 2 are realistically useful helps
  or harms the UX

If needed, refine the heuristic to favor variety and confidence over raw count.

---

### M2 — Cross-surface consistency

Autocomplete, `/play` submit responses, `/queue`, and now-playing should use a
shared naming style. Audit for drift such as:
- autocomplete shows uploader, queue shows artist_display, response shows title only
- duration appears in autocomplete but nowhere else
- source icon appears in one surface and not another

If inconsistency harms comprehension, propose or implement a shared formatting
module. The goal is a coherent "interaction language." 

---

### M3 — Ranking should match user intent, not merely score

A search result can be textually relevant yet still feel wrong. Audit ranking
with real queries:
- common tracks with many versions
- live versions vs studio versions
- covers vs originals
- title matches where artist differentiates intent

Explore whether lightweight UX-oriented ranking tweaks improve trust, such as:
- favor playable local/downloaded tracks over transient search stubs
- slightly favor exact title + artist matches over broader fuzzy matches
- de-prioritize very long or noisy uploader names for YouTube search stubs

Document suggestions even if you choose not to implement them now.

---

### M4 — URL preview should inspire confidence

When a user pastes a YouTube URL, the preview row is an important confidence
signal. `▶ YouTube: {video_id}` is technically fine, but not very human.

Audit opportunities to improve this without extra yt-dlp work in the hot path:
- better placeholder copy, e.g. `▶ YouTube video` or `▶ Play YouTube video`
- if cached metadata exists, ensure it surfaces title + artist/uploader + duration
- if only partial metadata exists, choose the most human-friendly subset

If the fallback placeholder is too mechanical, suggest a friendlier one.

---

### M5 — Mixed-source result presentation

Audit whether local and YouTube results are visually balanced. Too little
separation makes the list hard to reason about; too much separation makes it
feel fragmented.

Explore alternatives such as:
- keeping icons only (current baseline)
- slight source-based ordering rules within tied scores
- inserting a single soft hint row (only if it earns its place)

Do not add explicit section headers unless you can show they meaningfully help
within the 25-choice limit.

---

### M6 — Invisible background fetch may feel magical in a bad way

The repeated-query model is elegant technically, but users may experience it as:
"sometimes YouTube results show up, sometimes they don't."

Audit whether this behavior needs a subtle explanatory affordance. Candidates:
- special hint row after first no-local invocation
- hint in submit-time no-match response
- no change if testing shows the current behavior feels natural enough

Agents should think like product designers here: does the system teach itself
well enough, or is a minimal cue warranted?

---

## Optimization and Suggestion Areas

### O1 — Tighten copy everywhere

Audit every user-facing string in the `/play` search flow for:
- redundancy
- mechanical wording
- mismatched tone
- missing specificity

Example direction:
- Prefer `Added to queue: Never Gonna Give You Up — Rick Astley`
  over `Track added to queue successfully`
- Prefer `Only YouTube URLs are supported` over generic invalid-input errors

Propose a small copy guide if helpful.

---

### O2 — Smarter truncation

Naive truncation often removes the most helpful differentiator. Explore smarter
rules such as:
- always preserve as much of title as possible
- truncate uploader/artist before title when possible
- drop duration before dropping artist if row is too long
- avoid showing `— ` with no following text

Implement only if it materially improves scannability.

---

### O3 — Teach `yt:` gently

If the agent concludes `yt:` is too hidden, propose the lightest viable
teaching mechanism. Possibilities:
- first-time hint in no-match response
- contextual autocomplete hint row when local is empty
- one-time ephemeral tip after a failed local search

Any suggestion should minimize repeat annoyance for experienced users.

---

### O4 — Better submit-time reassurance

If a search result came from a transient YouTube search stub, the eventual play
may feel slower than a local result. Without exposing implementation details,
consider whether slightly richer submit feedback improves trust. For example:
- confirm full title + artist/uploader
- if a YouTube result was selected, consider whether a neutral response like
  `Queued: {title} — {artist}` is enough without source disclosure

Suggestions are welcome even if no code change is made in this pass.

---

## Self-Explore Areas

### S1 — Build a UX copy inventory

Collect all user-facing strings touched by `/play` search and categorize them:
- autocomplete rows
- URL preview rows
- no-match feedback
- unsupported URL errors
- submit success messages
- queue / now playing representations for the same track

Look for inconsistencies and propose a unified vocabulary.

---

### S2 — Prototype alternate row layouts

Without changing architecture, compare at least 2–3 display layouts using real
sample tracks, including:
- short local track
- long title with featured artists
- YouTube search result with uploader fallback
- title-only fallback

Judge them on speed of recognition, not just density.

---

### S3 — Audit from a new-user perspective

Pretend you know nothing about the bot. Try to answer:
- How would you discover `yt:`?
- Would you trust a `📺` row enough to click it?
- If nothing appears, would you blame the bot or your query?
- Does the system feel forgiving of imperfect spelling?

Document the rough edges a new user would hit first.

---

### S4 — Suggest future UX improvements

Even if not implemented now, record promising ideas for later passes, such as:
- one-tap "search YouTube" chip or pseudo-row
- richer source badges if Discord surfaces allow it
- a lightweight `/play` help hint in command description
- recent queries / recent plays integration

Keep suggestions realistic within Discord's interaction limits. [web:668][web:670]

---

## Verification Checklist

```bash
# Build and tests:
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace
cargo sqlx prepare --workspace

# Autocomplete scannability:
# Use queries that produce:
# - local-only results
# - mixed local + YouTube results
# - yt: only results
# - title-only / uploader fallback results
# Judge visually: are rows distinguishable in under 1 second?

# New-user discoverability:
# Try /play with an obscure query and no local matches
# Note what the user sees on first, second, third invocation
# Does the system teach itself enough?

# URL preview confidence:
# Paste a known cached YouTube URL
# Paste an uncached YouTube URL
# Compare whether the preview feels human or mechanical

# Response consistency:
# Select a local result, a downloaded YouTube result, and a youtube_search stub
# Compare the slash command follow-up messages
# Do they feel consistent and confidence-building?

# Edge formatting:
# Very long title, no artist, no duration, emoji/CJK title
# Check truncation, separators, and overall readability

# Mixed-source ergonomics:
# Search terms with many similar variants
# Does the result list help choose quickly, or feel cluttered/confusing?
```

---

## Output Requirements

At the end of this audit, produce:

1. **A UX findings list** grouped by UX-Critical, Major, Optimization, Explore
2. **A short product judgment**: does `/play` search currently feel polished,
   understandable, and trustworthy?
3. **Any UX refinements applied** with before/after rationale
4. **A suggestion list for later passes** if some improvements were deferred

If the implementation is already technically correct, spend remaining effort on:
- improving clarity and confidence
- reducing hidden affordances
- tightening copy and formatting
- aligning interaction feedback across surfaces
