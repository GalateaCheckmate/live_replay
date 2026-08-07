-- 旧版“添加主播”页面会把内置默认文件名写进每个主播，导致全局 filename_prefix
-- 永远无法对这些主播生效。只清理这个由 UI 自动写入的精确旧默认值；真正自定义的
-- filename_prefix 保持不变。
UPDATE livestreamers
SET filename_prefix = NULL
WHERE filename_prefix = '{streamer}%Y-%m-%dT%H_%M_%S';
