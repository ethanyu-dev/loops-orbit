SELECT d.day, count(*) AS count
FROM communication_documents d
JOIN communication_sources s ON s.id = d.source_id
WHERE s.owner = $1 AND ($2::text IS NULL OR d.day < $2)
GROUP BY d.day
ORDER BY d.day DESC
LIMIT $3
