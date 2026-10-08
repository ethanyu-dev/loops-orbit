SELECT EXISTS(SELECT 1
FROM runs r
JOIN conversations c ON c.id=r.conversation_id
WHERE r.id=$1
  AND r.lease_token=$2
  AND r.status='running'
  AND c.owner='admin');
