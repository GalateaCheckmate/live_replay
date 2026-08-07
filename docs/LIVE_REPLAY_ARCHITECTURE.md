# Live Replay architecture

Live Replay is being separated from the historical biliup application shell. The goal is not to rename biliup concepts, but to make recording and replay submission a product with its own stable domain model.

## User model

The main UI exposes only four current states for a streamer:

- `waiting` — enabled and waiting for the next live session (or disabled via the separate switch)
- `recording` — a live session is currently recording
- `uploading` — recording is not active, but one or more segments still need upload, remote verification, or cleanup
- `error` — a conflict or uncertain first submission requires user action

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

## Migration rule

Each refactor should preserve recording safety first:

1. introduce a Live Replay-owned interface/domain model;
2. move UI/API consumers to that interface;
3. isolate the old implementation behind an adapter;
4. only then delete the corresponding legacy biliup shell code.

This prevents a cosmetic rename from becoming a risky rewrite of the proven recorder/upload safety path.

## Android

The Android prototype should use the same domain concepts and four-state model, but implement Android-specific process survival, foreground service, storage and network behavior. It should not copy the legacy Web task/config model.
