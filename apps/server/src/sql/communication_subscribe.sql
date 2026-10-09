INSERT INTO communication_sources(id, owner, chat_id, label, start_at, watermark, day_timezone)
VALUES ($1, 'admin', $2, $3, $4, $4, 'Asia/Shanghai')
ON CONFLICT(owner, chat_id) DO UPDATE SET
    label = excluded.label,
    subscribed = true,
    enabled = CASE WHEN communication_sources.subscribed THEN communication_sources.enabled ELSE true END,
    version = communication_sources.version + CASE WHEN communication_sources.subscribed THEN 0 ELSE 1 END,
    start_at = CASE WHEN communication_sources.subscribed THEN communication_sources.start_at ELSE excluded.start_at END,
    watermark = CASE WHEN communication_sources.subscribed THEN communication_sources.watermark ELSE excluded.watermark END,
    page_token = CASE WHEN communication_sources.subscribed THEN communication_sources.page_token ELSE '' END,
    window_start = CASE WHEN communication_sources.subscribed THEN communication_sources.window_start ELSE NULL END,
    window_end = CASE WHEN communication_sources.subscribed THEN communication_sources.window_end ELSE NULL END,
    next_sync = now()
WHERE NOT communication_sources.removal_pending
RETURNING id
