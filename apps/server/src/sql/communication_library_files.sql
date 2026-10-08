SELECT d.id, d.source_id, s.label AS source_label, s.enabled AS source_enabled, s.subscribed AS source_subscribed,
       d.day, d.version, d.extraction_version, d.summary_hash, d.summary_error
FROM communication_documents d
JOIN communication_sources s ON s.id = d.source_id
WHERE s.owner = $1
  AND ($2::text IS NULL OR d.day = $2)
  AND ($3 = '' OR strpos(lower(s.label), lower($3)) > 0 OR strpos(d.day, $3) > 0)
ORDER BY d.day DESC, s.label, d.id
LIMIT $4 OFFSET $5
