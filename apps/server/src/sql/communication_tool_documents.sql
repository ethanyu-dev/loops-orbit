SELECT d.id, d.source_id, d.day, d.raw_hash, d.version, d.extraction_version,
       d.summary_hash, d.summary_error, s.label, s.enabled, s.subscribed
FROM communication_documents d
JOIN communication_sources s ON s.id=d.source_id
WHERE s.owner IN (
    SELECT owner FROM communication_connections WHERE owner=$1 OR 'feishu:'||open_id=$1
)
  AND ($2::text IS NULL OR d.day >= $2)
  AND ($3::text IS NULL OR d.day <= $3)
  AND ($4::uuid IS NULL OR d.source_id=$4)
  AND ($5::uuid IS NULL OR d.id=$5)
ORDER BY d.day DESC, d.id
