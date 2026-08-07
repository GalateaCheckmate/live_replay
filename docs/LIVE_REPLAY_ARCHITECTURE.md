# Live Replay architecture

Live Replay is being separated from the historical biliup application shell. The goal is not to rename biliup concepts, but to make recording and replay submission a product with its own stable domain model.

## User model

The main UI exposes only four current states for a streamer:

- `waiting` — enabled and waiting for the next live session (or disabled via the separate switch)
- `recording` — a live session is currently recording
- `uploading` — recording is not active, but one or more segments still need upload, remote verification, or cleanup
- `error` — a conflict or uncertain first submission requires user action

Completed sessions may be labelled `complete`/“已完成” in history views, but this is not a fifth current streamer state.

Internal downloader/uploader states remain implementation details and must not be used by new UI or future Android code.

## Domain model

### Streamer

Persistent configuration for whom to monitor. It does not own historical recording state.

### RecordingSession

One real live broadcast. A reconnect inside the configured reconnect window may resume the same session; a later broadcast creates a new session.

A session owns ordered `RecordingSegment` records and maps to at most one Bilibili submission.

### RecordingSegment

A durable local file unit produced by the recorder. A completed segment is persisted before it is eligible for upload. Segment lifecycle and retry metadata are stored in SQLite.

### UploadQueue

Upload execution is independent from recording. Recorder completion only persists a segment and wakes the queue. Upload failure must never stop the next recording segment.

### Submission

The remote Bilibili identity for a session (`aid` / `bvid`). The first segment creates the submission; later segments append in part order. Uncertain first submissions are stopped for manual reconciliation to prevent duplicate submissions.

### Storage

Local files are deleted only after remote playback verification. Files remain recoverable after process crashes through SQLite state plus the filesystem outbox.

## Persistence

The current safe schema already represents the required model:

- `livestreamers` — Streamer configuration
- `live_sessions` — RecordingSession + submission identity/policy snapshot
- `recording_segments` — durable segment lifecycle
- `upload_jobs` — retry/queue execution metadata
- `replay_outbox` — crash-safe bridge between completed files and SQLite

Do not create a second parallel queue/schema unless the existing tables are migrated atomically.

The current migration keeps `livestreamers` and `uploadstreamers` as compatibility storage for streamer/submission defaults. New clients do not see the historical upload-template/override object graph: the Replay API projects it into a small `ReplayStreamerSettings` model and preserves fields that have not been migrated yet.

## Replay API boundary

New Web and Android clients should stay in the `/v1/replay/*` namespace.

Current product-facing endpoints include:

- `GET/POST /v1/replay/streamers` — current streamer state / add streamer
- `PUT /v1/replay/streamers/{id}/enabled` — explicit idempotent enable/disable
- `GET/PUT /v1/replay/streamers/{id}/settings` — clean streamer/recording/submission defaults
- `GET /v1/replay/activity` — clean Session/Segment summaries without raw worker/job states
- `GET/PUT /v1/replay/settings` — global Live Replay settings
- `GET /v1/replay/storage` — recording storage protection state
- replay session/job action endpoints used for retry and uncertain-submission reconciliation

Legacy `/v1/streamers`, `/v1/upload/streamers` and generic configuration routes remain temporarily for backend compatibility and old tooling. New product UI must not depend on them.

## Boundaries

The intended dependency direction is:

```text
PC Web UI / future Android UI
              |
        Replay domain API
              |
  Monitor -> Recorder -> SQLite -> UploadQueue -> Submission adapter
                         |
                       Storage
```

Platform parsing and Bilibili protocol implementations are adapters. They may currently come from the existing Rust `biliup` crate, but the Replay domain must not expose their task enums, configuration objects, or UI concepts.

## Recording-start race safety

A live-status request can be in flight while the user disables a streamer or saves new settings. The request result must not be trusted just because it originally belonged to a valid Worker.

Immediately before `start_download_workflow` changes a Worker to `Working`, the recorder now re-checks:

1. the worker is still registered in Monitor;
2. the registered `Arc<Worker>` is the same instance that produced the live result;
3. the worker has not been paused;
4. `livestreamers.enabled` is still true in SQLite;
5. recording disk protection still allows a new recording.

This prevents a late live-check response from starting a removed/replaced/disabled Worker with stale settings.

## Migration rule

Each refactor should preserve recording safety first:

1. introduce a Live Replay-owned interface/domain model;
2. move UI/API consumers to that interface;
3. isolate the old implementation behind an adapter;
4. only then delete the corresponding legacy biliup shell code.

This prevents a cosmetic rename from becoming a risky rewrite of the proven recorder/upload safety path.

## Current migration status

Completed in the first Replay-first refactor:

- main navigation and primary pages no longer expose upload templates, Jobs or raw worker status;
- legacy upload-template, Job and raw-status routes were removed from the Next.js product UI;
- streamer cards and streamer editing use Replay-owned APIs;
- user-facing state is reduced to four states;
- Session/Segment activity is aggregated by the backend instead of interpreted by the frontend;
- recording/upload durability continues to use the existing safe SQLite/outbox schema;
- stale in-flight live checks are rejected before recording starts.

Still intentionally behind a compatibility boundary:

- the proven Rust recording/session queue remains in `common/replay.rs`;
- Bilibili login, upload-line selection, file upload, submission/append and remote verification still use types from the existing Rust `biliup` crate;
- the next backend extraction should wrap those calls in a Live Replay `BilibiliSubmissionAdapter` (or equivalent interface) before deleting the remaining protocol implementation imports.

Do not delete that protocol code merely to remove the word `biliup`: preserving upload identity checks, uncertain-first-submission protection, remote playability verification and crash-safe cleanup takes priority.

## Android

The Android prototype should use the same domain concepts and four-state model, but implement Android-specific process survival, foreground service, storage and network behavior. It should not copy the legacy Web task/config model.
