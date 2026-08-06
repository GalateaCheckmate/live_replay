PRAGMA foreign_keys = ON;

ALTER TABLE live_sessions ADD COLUMN session_key TEXT;
ALTER TABLE live_sessions ADD COLUMN upload_config_json TEXT;
ALTER TABLE live_sessions ADD COLUMN submit_state TEXT NOT NULL DEFAULT 'new';
ALTER TABLE live_sessions ADD COLUMN submit_token TEXT;
ALTER TABLE live_sessions ADD COLUMN delete_after_success INTEGER NOT NULL DEFAULT 0;
ALTER TABLE live_sessions ADD COLUMN preserve_danmaku INTEGER NOT NULL DEFAULT 1;
ALTER TABLE live_sessions ADD COLUMN next_part_to_upload INTEGER NOT NULL DEFAULT 1;
ALTER TABLE live_sessions ADD COLUMN last_activity_at DATETIME;
UPDATE live_sessions SET last_activity_at = COALESCE(updated_at, CURRENT_TIMESTAMP)
WHERE last_activity_at IS NULL;

ALTER TABLE recording_segments ADD COLUMN original_file_path TEXT;
ALTER TABLE recording_segments ADD COLUMN file_identity TEXT;
ALTER TABLE recording_segments ADD COLUMN file_mtime_ns INTEGER;
ALTER TABLE recording_segments ADD COLUMN remote_filename TEXT;
ALTER TABLE recording_segments ADD COLUMN cleanup_state TEXT NOT NULL DEFAULT 'pending';

-- 兼容前一版 Live Replay 状态。旧 verified 已经通过远端检查，但没有保存
-- remote_filename，不能再安全地自动核对或删除，因此升级后明确保留本地文件。
UPDATE recording_segments
SET status = 'retained', cleanup_state = 'retained', last_error = NULL
WHERE status = 'verified';

UPDATE upload_jobs
SET status = 'complete', last_error = NULL, locked_at = NULL, updated_at = CURRENT_TIMESTAMP
WHERE segment_id IN (
    SELECT id FROM recording_segments WHERE status IN ('deleted', 'retained')
);

-- 从第一个非终态分P继续；没有待处理分P时指向 expected_parts + 1。
UPDATE live_sessions
SET next_part_to_upload = COALESCE(
        (SELECT MIN(r.part_number)
         FROM recording_segments r
         WHERE r.session_id = live_sessions.id
           AND r.status NOT IN ('deleted', 'retained')),
        expected_parts + 1
    ),
    verified_parts = COALESCE(
        (SELECT MAX(r.part_number)
         FROM recording_segments r
         WHERE r.session_id = live_sessions.id
           AND r.status IN ('deleted', 'retained')),
        verified_parts
    );

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

-- 只要存在未完成场次或待入库 outbox，就禁止删除主播。这样即使 SQLite
-- 短暂不可写、录像只落在文件系统 outbox 中，外键级联也不会把恢复锚点删掉。
CREATE TRIGGER IF NOT EXISTS protect_pending_replay_streamer_delete
BEFORE DELETE ON livestreamers
WHEN EXISTS (
    SELECT 1 FROM live_sessions s
    WHERE s.live_streamer_id = OLD.id
      AND (
        s.status != 'complete'
        OR EXISTS (
            SELECT 1 FROM recording_segments r
            WHERE r.session_id = s.id AND r.status NOT IN ('deleted', 'retained')
        )
        OR EXISTS (SELECT 1 FROM replay_outbox o WHERE o.session_id = s.id)
      )
)
BEGIN
    SELECT RAISE(ABORT, '该主播仍有未完成的 Live Replay 上传任务，不能删除');
END;

CREATE TRIGGER IF NOT EXISTS protect_referenced_upload_template_delete
BEFORE DELETE ON uploadstreamers
WHEN EXISTS (SELECT 1 FROM livestreamers WHERE upload_streamers_id = OLD.id)
BEGIN
    SELECT RAISE(ABORT, '投稿模板仍被主播使用，请先解除关联');
END;

CREATE TRIGGER IF NOT EXISTS protect_pending_streamerinfo_delete
BEFORE DELETE ON streamerinfo
WHEN EXISTS (
    SELECT 1 FROM live_sessions s
    WHERE s.source_streamer_info_id = OLD.id
      AND (
        s.status != 'complete'
        OR EXISTS (
            SELECT 1 FROM recording_segments r
            WHERE r.session_id = s.id AND r.status NOT IN ('deleted', 'retained')
        )
        OR EXISTS (SELECT 1 FROM replay_outbox o WHERE o.session_id = s.id)
      )
)
BEGIN
    SELECT RAISE(ABORT, '该历史记录仍有关联的未完成上传任务');
END;
