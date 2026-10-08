SELECT count(*)
FROM communication_documents d
JOIN communication_sources s ON s.id = d.source_id
LEFT JOIN communication_document_progress p ON p.document_id=d.id AND p.version=d.version
  AND d.extraction_version=1 AND p.summary_hash IS NOT DISTINCT FROM d.summary_hash
  AND p.summary_error IS NOT DISTINCT FROM d.summary_error
WHERE s.owner = $1
  AND ($2::text IS NULL OR d.day = $2)
  AND ($3 = '' OR strpos(lower(s.label), lower($3)) > 0 OR strpos(d.day, $3) > 0)
  AND ($4 = 'all' OR ($4 = 'failed' AND (d.summary_error IS NOT NULL OR p.status='errors' OR p.images_failed>0)) OR ($4 = 'images_failed' AND p.images_failed>0))
