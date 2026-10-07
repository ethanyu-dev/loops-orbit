-- 已执行提及重处理迁移的安装也必须在启动时清理一次旧上下文。
-- 隔离中的资料之后逐日升级，不再推进遗忘边界，避免取消升级后的新聊天。
UPDATE communication_connections c SET scope_context_pending=true
WHERE EXISTS (
    SELECT 1 FROM communication_sources s
    JOIN communication_documents d ON d.source_id=s.id
    WHERE s.owner=c.owner AND d.extraction_version=0
);
