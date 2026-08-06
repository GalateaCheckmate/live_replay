PRAGMA foreign_keys = ON;

ALTER TABLE live_sessions ADD COLUMN session_key TEXT;
ALTER TABLE live_sessions ADD COLUMN upload_config_json TEXT;
ALTER TABLE live_sessions ADD COLUMN submit_state TEXT NOT NULL DEFAULT 'new';
ALTER TABLE live_sessions ADD COLUMN submit_token TEXT;
ALTER TABLE live_sessions ADD COLUMN delete_after_success INTEGER NOT NULL DEFAULT 0;
ALTER TABLE live_sessions ADD COLUMN preserve_danmaku INTEGER NOT NULL DEFAULT 1;
ALTER TABLE live_sessions ADD COLUMN next_part_to_upload INTEGER NOT NULL DEFAULT 1;
ALTER TABLE live_sessions ADD COLUMN last_activity_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP;

ALTER TABLE recording_segments ADD COLUMN original_file_path TEXT;
ALTER TABLE recording_segments ADD COLUMN file_identity TEXT;
ALTER TABLE recording_segments ADD COLUMN file_mtime_ns INTEGER;
ALTER TABLE recording_segments ADD COLUMN remote_filename TEXT;
ALTER TABLE recording_segments ADD COLUMN cleanup_state TEXT NOT NULL DEFAULT 'pending';

CREATE UNIQUE INDEX IF NOT EXISTS idx_live_sessions_session_key
    ON live_sessions(session_key) WHERE session_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_live_sessions_pending_part
    ON live_sessions(status, next_part_to_upload, id);
CREATE INDEX IF NOT EXISTS idx_recording_segments_remote_filename
    ON recording_segments(session_id, remote_filename);

CREATE TABLE IF NOT EXISTS replay_outbox
(
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    session_id INTEGER NOT NULL REFERENCES live_sessions(id) ON DELETE CASCADE,
    part_number INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    original_file_path TEXT NOT NULL,
    danmaku_file_path TEXT,
    file_size INTEGER NOT NULL,
    file_mtime_ns INTEGER NOT NULL,
    file_identity TEXT NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(session_id, part_number),
    UNIQUE(file_path)
);

-- 未完成任务存在时，禁止删除监控主播。否则原有级联关系会把上传队列一起删掉。
CREATE TRIGGER IF NOT EXISTS protect_pending_replay_streamer_delete
BEFORE DELETE ON livestreamers
WHEN EXISTS (
    SELECT 1
    FROM live_sessions s
    JOIN recording_segments r ON r.session_id = s.id
    WHERE s.live_streamer_id = OLD.id
      AND r.status NOT IN ('deleted', 'retained')
)
BEGIN
    SELECT RAISE(ABORT, '该主播仍有未完成的 Live Replay 上传任务，不能删除');
END;

-- 投稿模板被主播引用时禁止直接删除，避免 ON DELETE CASCADE 连带删除主播及队列。
CREATE TRIGGER IF NOT EXISTS protect_referenced_upload_template_delete
BEFORE DELETE ON uploadstreamers
WHEN EXISTS (SELECT 1 FROM livestreamers WHERE upload_streamers_id = OLD.id)
BEGIN
    SELECT RAISE(ABORT, '投稿模板仍被主播使用，请先解除关联');
END;

-- 历史记录仍被未完成队列引用时禁止删除。
CREATE TRIGGER IF NOT EXISTS protect_pending_streamerinfo_delete
BEFORE DELETE ON streamerinfo
WHEN EXISTS (
    SELECT 1
    FROM live_sessions s
    JOIN recording_segments r ON r.session_id = s.id
    WHERE s.source_streamer_info_id = OLD.id
      AND r.status NOT IN ('deleted', 'retained')
)
BEGIN
    SELECT RAISE(ABORT, '该历史记录仍有关联的未完成上传任务');
END;
