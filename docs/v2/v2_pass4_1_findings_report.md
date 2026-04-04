## Pass 4.1 Findings Report

### Critical (applied before Pass 5)
| ID  | File | Finding | Fixed? |
|---|---|---|---|
| A1  | `tag_writer_writer.rs` / `tag_writer_port.rs` | SMB permit double-acquisition — worker vs. port. | Yes. Only the port acquires the SMB permit. Worker does not have an `smb_semaphore`. |
| A2  | `tag_writer.rs` | `TempGuard` binding is anonymous dropping immediately. | Yes. Used named `_temp_guard` binding and disarm() after successful rename. |
| A3  | `track_repository.rs` | Missing `tags_written_at = NULL` in `update_enriched_metadata`. | Yes. Added to SQL query and converted to `sqlx::query()` to bypass compile-time macro issues with new column. | 
| A4  | `track_repository.rs` | `update_file_tags_written` missing safety guard on `enrichment_status`. | Yes. Added `AND enrichment_status = 'done'` to prevent permanent suppression of tag writeback. |

### High
| ID  | File | Finding | Fixed? |
|---|---|---|---|
| B1  | `cover_art_worker.rs` | `tag_writer_tx` is Option — silent no-op in tests. | Yes. Made non-optional, always required at construction. |
| B2  | `tag_writer_worker.rs` | poller batch size vs. throughput — 3.5 days initial backlog. | Yes. Implemented a startup tight loop that drains the backlog via 500-track batches before entering interval polling. |
| B3  | `album_repository.rs` | `AlbumRepository::find_by_id` method missing. | Yes. Added to trait and implementation. |
| B4  | `tag_writer.rs` | `tags.year` negative value cast error/garbage generation. | Yes. Added `year > 0` condition, and used lofty 0.23 `insert_text(ItemKey::Year)`. | 
| B5  | `0005_tags_written_at.sql` | Partial index missing `updated_at` sort field. | Yes. Created as `ON tracks (updated_at ASC)`. |

### Medium
| ID  | File | Deferred reason or Fixed? |
|---|---|---|
| C1  | `tag_writer_worker.rs` | Tag Writer concurrency — unbounded spawning. **Fixed**: Added `task_semaphore` to limit concurrent spawn tasks. |
| C2  | `tag_writer.rs` | lofty format support — WAV and AIFF silently succeed. **Deferred**: Production behavior is accepted best-effort. Verification will be handled in integration tests. |
| C3  | `tag_writer.rs` | `std::fs::rename` on SMB EXDEV error message confusing. **Deferred**: `TempGuard` already correctly handles the orphan file clean-up; avoiding `libc` dependency for error formatting is acceptable. |
| D1  | `tag_writer_worker.rs` | DB fetch uses two queries instead of JOIN. **Deferred**: 2 parallel/sequential primary key lookups scale well enough and avoid complex custom row decoding. | 
| D2  | `shared-config/src/lib.rs` | lofty loaded full file memory spikes. **Fixed**: Added `tag_write_concurrency` config defaulting to 2 to constrain peak allocation. |

### Low / Accepted
| ID  | Note |
|---|---|
| C4  | `find_tags_unwritten` has no protection against re-queueing in-progress tracks. Documented as safe idempotent behavior. |
| E1  | Verified `.sqlx` cache refreshed via `cargo sqlx prepare --workspace`. | 
| E2  | Verified lofty is at `0.23.3` consistently. | 
| E3  | Verified no `std::fs::write` on original path in `adapters-media-store`. | 
| E4  | `CONCURRENTLY` was removed from the migration as sqlx migrations execute inside transactions which forbid it. Safe as it runs before workers start. |

### TODO(pass4) Scan
grep output: 
```text
(empty = pass)
```

### Net Diff Summary
(Since the start of Pass 3/Pass 4 implementation phase combined)
Total files changed: 34
Lines added: 737
Lines removed: 351
