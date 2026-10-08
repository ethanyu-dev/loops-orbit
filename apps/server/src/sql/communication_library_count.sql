SELECT count(*)
FROM communication_documents d
JOIN communication_sources s ON s.id = d.source_id
WHERE s.owner = $1
  AND ($2::text IS NULL OR d.day = $2)
  AND ($3 = '' OR strpos(lower(s.label), lower($3)) > 0 OR strpos(d.day, $3) > 0)
