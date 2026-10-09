INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone,chat_mode)
SELECT $1,$2,$3,$4,$5,$5,'Asia/Shanghai','p2p'
WHERE NOT EXISTS(SELECT 1 FROM communication_exclusions WHERE owner=$2 AND chat_id=$3)
-- 来源存在不等于用户拒绝自动订阅；只有未排除、未订阅的历史来源可以恢复。
ON CONFLICT(owner,chat_id) DO UPDATE SET
    subscribed = true,
    enabled = true,
    chat_mode = 'p2p',
    version = communication_sources.version + 1,
    -- 从自动订阅边界补采遗漏消息，同时保留更晚的来源起点，避免扩大历史范围。
    start_at = GREATEST(communication_sources.start_at, excluded.start_at),
    watermark = GREATEST(communication_sources.start_at, excluded.start_at),
    page_token = '',
    window_start = NULL,
    window_end = NULL,
    error = NULL,
    next_sync = now()
WHERE NOT communication_sources.subscribed
