PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS live_sessions
(
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    live_streamer_id INTEGER NOT NULL
        REFERENCES livestreamers(id) ON DELETE CASCADE,
    source_streamer_info_id INTEGER NOT NULL
        REFERENCES streamerinfo(id) ON DELETE CASCADE,
    streamer_name TEXT NOT NULL,
    streamer_url TEXT NOT NULL,
    live_title TEXT NOT NULL,
    started_at DATETIME NOT NULL,
    ended_at DATETIME,
    status TEXT NOT NULL DEFAULT 'recording',
    aid INTEGER,
    bvid TEXT,
    expected_parts INTEGER NOT NULL DEFAULT 0,
    verified_parts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_live_sessions_streamer_status
    ON live_sessions(live_streamer_id, status, id DESC);
CREATE INDEX IF NOT EXISTS idx_live_sessions_remote
    ON live_sessions(aid, bvid);

CREATE TABLE IF NOT EXISTS recording_segments
(
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    session_id INTEGER NOT NULL
        REFERENCES live_sessions(id) ON DELETE CASCADE,
    part_number INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    processed_file_path TEXT,
    danmaku_file_path TEXT,
    file_size INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'queued',
    uploaded_filename TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    next_retry_at DATETIME,
    uploaded_at DATETIME,
    verified_at DATETIME,
    deleted_at DATETIME,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(session_id, part_number),
    UNIQUE(file_path)
);

CREATE INDEX IF NOT EXISTS idx_recording_segments_session_status
    ON recording_segments(session_id, status, part_number);

CREATE TABLE IF NOT EXISTS upload_jobs
(
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    segment_id INTEGER NOT NULL UNIQUE
        REFERENCES recording_segments(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    next_attempt_at DATETIME,
    locked_at DATETIME,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_upload_jobs_ready
    ON upload_jobs(status, next_attempt_at, id);
