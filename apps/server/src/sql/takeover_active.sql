-- 代际阻止删除重连后的版本复用；短时间内已发送或更新输入的会话不再处理旧消息。
SELECT EXISTS(
    SELECT 1 FROM communication_takeover_jobs j
    JOIN communication_sources s ON s.id=j.source_id
    JOIN communication_connections c ON c.owner=s.owner
    JOIN communication_takeover_settings t ON t.owner=c.owner
    WHERE j.id=$1 AND j.status='evaluating' AND s.id=$2 AND s.version=$3
      AND s.enabled AND s.subscribed AND NOT s.removal_pending AND s.chat_mode='p2p'
      AND c.status='active' AND c.send_authorized AND c.version=$4 AND c.private_discovery_generation=$5
      AND t.enabled AND t.version=$6
      AND j.message->>'chat_id'=s.chat_id
      -- 真实本人身份保留在消息中；只在服务端绑定的自聊中允许模拟外部提问。
      AND (j.message->>'is_me'='false' OR
           (j.message->>'is_me'='true' AND t.self_test_chat_id=s.chat_id AND j.message->>'sender_id'=c.open_id))
      AND (j.message->>'create_time')::bigint >= t.since_ms
      AND NOT EXISTS(SELECT 1 FROM communication_takeover_jobs newer WHERE newer.source_id=s.id
          AND ((newer.message->>'create_time')::bigint > (j.message->>'create_time')::bigint
            OR (newer.status='sent' AND newer.updated_at>now()-interval '30 seconds')))
)
