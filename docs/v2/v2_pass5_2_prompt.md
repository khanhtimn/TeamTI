# TeamTI v2 — Pass 5.2 Prompt
## Autonomous Correctness & Architecture Review: Discord Integration

> This is an open-ended audit pass. Unlike previous review passes, findings
> are NOT pre-enumerated. The agent must independently identify, evaluate,
> and propose solutions for any correctness or architectural issues found.
> Do not apply fixes speculatively — report first, then wait for approval
> unless a finding is clearly a compile error or critical safety issue.

---

### Purpose

Passes 5 and 5.1 were written sequentially with incremental guidance. This
pass steps back and asks: **is the Discord integration section correct and
well-architected as a whole?**

The agent should treat this as a greenfield audit — read all relevant files,
compare them against the baseline requirements below, and produce an honest
assessment. Prior pass prompts should be treated as implementation intent,
not as ground truth. If the implementation diverges from prior prompts in a
way that is actually better, say so.

---

### Baseline Requirements

These are the non-negotiable correctness and architectural properties that
the Discord integration must satisfy. Evaluate the current implementation
against each one.

#### R1 — Functional Correctness
The following user flows must work without exceptions, panics, stuck state,
or silent failures:

1. **Happy path:** User in voice channel → `/play <query>` → bot joins →
   audio plays → track ends → next queued track starts automatically →
   queue exhausted → bot leaves after `AUTO_LEAVE_SECS`

2. **Queue during timer:** Bot is waiting to leave (queue empty) → user
   calls `/play` → timer cancels → audio plays

3. **Channel move:** Bot playing in Channel A → user in Channel B calls
   `/play` → bot moves to Channel B → queue preserved → new track appended
   → playback continues without stuck state

4. **Hard stop:** User calls `/leave` → playback stops immediately →
   queue cleared → bot leaves → no orphaned handlers or timers

5. **Bad file:** A track in the queue references a missing or corrupted
   file → that track is skipped with a log warning → next track in queue
   plays → bot does not get stuck

6. **Error recovery:** Any single Songbird, HTTP, or I/O error in the
   playback path does not permanently stall the queue

#### R2 — Architectural Consistency
The Discord integration must be consistent with the rest of the v2 codebase:

1. **Error types:** All public-facing errors use `AppError` from
   `application/src/error.rs`. No separate error enums are exposed across
   crate boundaries. Internal-only error types are `pub(crate)`.

2. **Structured logging:** All `warn!` and `error!` calls include
   `error = %e`, `error.kind = e.kind_str()`, and `operation = "..."` as
   structured fields. No dynamic data embedded in message strings.

3. **Dependency versions:** `serenity` and `songbird` are declared once
   in `[workspace.dependencies]` using the same git references established
   in v1. Crate-level Cargo.toml files use `{ workspace = true }`. No
   version strings (e.g. `"0.12"`) appear alongside git references.

4. **Cancellation:** All long-running tasks (auto-leave timer, any polling
   loop) respect the application-level `CancellationToken` from Pass 2.

#### R3 — Queue Architecture Justification
The implementation makes a choice between:
- **Songbird's built-in `TrackQueue`** (enabled via `builtin-queue` feature)
- **A custom `VecDeque<QueuedTrack>`** driven manually
- **A hybrid approach** (Songbird handles audio advancement; custom structure
  holds domain metadata in parallel)

Evaluate which approach is actually implemented. Then assess:
- Is the chosen approach internally consistent? (No mix of approaches without
  explicit coordination)
- Does the chosen approach support Pass 6 requirements (skip, pause, resume)
  without requiring a full rewrite?
- Are there race conditions or event-handling gaps specific to the chosen approach?
- If the approach should change, propose the alternative and justify it.

#### R4 — Thread Safety
`GuildMusicState` is shared across async tasks. Evaluate:
- Are all mutations to shared state done while holding the `Mutex`?
- Are there any `.await` points that hold the `MutexGuard` across a yield
  (which would deadlock in a single-threaded executor context)?
- Is there any TOCTOU pattern where a check and a subsequent action are
  not atomic with respect to concurrent tasks?

#### R5 — User Experience Consistency
- All commands must respond within Discord's 3-second interaction window,
  or defer immediately with `defer_ephemeral()`
- Error messages shown to users must be actionable (tell the user what to do)
  not diagnostic (do not expose internal error types or stack traces)
- The now-playing message must always reflect the current state after any
  state transition

---

### Files to Examine

Read all of the following before writing any findings:

```
crates/adapters-voice/src/state.rs
crates/adapters-voice/src/player.rs
crates/adapters-voice/src/track_end_handler.rs
crates/adapters-voice/src/error.rs          (may not exist after 5.1)
crates/adapters-voice/src/lib.rs
crates/adapters-voice/Cargo.toml
crates/adapters-discord/src/commands/play.rs
crates/adapters-discord/src/commands/clear.rs
crates/adapters-discord/src/commands/leave.rs
crates/adapters-discord/src/commands/rescan.rs
crates/adapters-discord/src/handler.rs
crates/adapters-discord/src/lib.rs
crates/adapters-discord/Cargo.toml
crates/application/src/error.rs
apps/bot/src/main.rs
Cargo.toml                                   (workspace manifest)
```

Also run the following and include output in the report:

```bash
# 1. Check for duplicate dependency versions
cargo tree | grep -E "^(serenity|songbird|dashmap)"

# 2. Check for version strings alongside git refs
grep -rn "serenity\|songbird" \
    crates/adapters-voice/Cargo.toml \
    crates/adapters-discord/Cargo.toml

# 3. Check for remaining v1 commands
grep -rn "\"ping\"\|fn ping\|\"help\"\|fn help" --include="*.rs" .

# 4. Check for AppError violations (separate error enums crossing boundaries)
grep -rn "pub enum.*Error\|pub struct.*Error" \
    --include="*.rs" \
    crates/adapters-voice/src/ \
    crates/adapters-discord/src/

# 5. Check for MutexGuard held across await points
grep -n "\.lock()\." --include="*.rs" -r \
    crates/adapters-voice/src/ \
    crates/adapters-discord/src/

# 6. Check for interactions without defer
grep -n "fn run\b" --include="*.rs" -r \
    crates/adapters-discord/src/commands/
```

---

### Evaluation Process

Work through the requirements in order: R1 → R2 → R3 → R4 → R5.

For each requirement:
1. State whether the current implementation **satisfies**, **partially satisfies**,
   or **does not satisfy** the requirement
2. If not fully satisfied: quote the specific code that causes the issue
3. Propose a fix, and classify it as:
   - `BLOCK` — must be fixed before any user testing
   - `HIGH` — should be fixed before Pass 6
   - `MEDIUM` — should be addressed but does not block progress
   - `LOW` — cosmetic or minor; can be deferred

For R3 (queue architecture): provide a dedicated section with a comparison
table of the three approaches against Pass 5 and Pass 6 requirements, then
make a clear recommendation with justification.

---

### Report Format

```markdown
# Pass 5.2 Audit Report

## Tool Output
### cargo tree (serenity/songbird/dashmap)
<output>

### Dependency version check
<output>

### v1 command check
<output>

### Error boundary check
<output>

### MutexGuard-across-await check
<output>

### Defer check
<output>

---

## R1 — Functional Correctness
### Flow 1: Happy path
Status: PASS / PARTIAL / FAIL
Issue (if any): ...
Proposed fix: ...
Classification: BLOCK / HIGH / MEDIUM / LOW

### Flow 2: Queue during timer
...

### Flow 3: Channel move
...

### Flow 4: Hard stop
...

### Flow 5: Bad file
...

### Flow 6: Error recovery
...

---

## R2 — Architectural Consistency
### R2.1 Error types
Status: ...

### R2.2 Structured logging
Status: ...

### R2.3 Dependency versions
Status: ...

### R2.4 Cancellation
Status: ...

---

## R3 — Queue Architecture

### Current approach
<describe what is actually implemented>

### Comparison table

| Criterion | Custom VecDeque | Songbird Built-in | Hybrid |
|---|---|---|---|
| Pass 5 requirements met | | | |
| Pass 6 skip/pause/resume | | | |
| Channel move safety | | | |
| TrackEvent handling | | | |
| Code complexity | | | |
| Risk of race conditions | | | |

### Recommendation
<clear recommendation with justification>

---

## R4 — Thread Safety
<findings per state access pattern>

---

## R5 — User Experience
<findings per command>

---

## Summary

### BLOCK items
| ID | Requirement | Issue | Fix |

### HIGH items
| ID | Requirement | Issue | Fix |

### MEDIUM items
| ID | Issue |

### LOW items
| ID | Issue |

### Estimated effort to reach BLOCK-clear state
<lines changed, files affected>
```

---

### Constraints on the Agent

- Do not apply fixes autonomously. Report and wait for approval, except for
  obvious compile errors that prevent `cargo check` from running.
- Do not introduce new features, commands, or pipeline stages.
- If you find that a prior pass prompt contained incorrect guidance (wrong
  API, wrong architecture), say so explicitly — do not silently implement
  the wrong thing.
- If the implementation already satisfies a requirement correctly, say
  PASS and move on. Do not manufacture findings.

---

### REFERENCE

docs/v2/v2_master.md

git refs for serenity/songbird (consult v1 docs)
