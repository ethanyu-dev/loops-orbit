INSERT INTO linear_operations(operation_key,run_id,generation,issue_id,status)
VALUES ($1,$2,$3,$4,'dispatching')
ON CONFLICT  DO NOTHING;
