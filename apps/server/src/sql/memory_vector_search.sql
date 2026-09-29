-- CASE 避免查询计划提前计算其他模型空间的距离，兼容维度变化。
SELECT id, content_hash
FROM memory_vectors
WHERE owner = $1
  AND CASE WHEN version = $2
      THEN 1 - (embedding OPERATOR(public.<=>) $3::public.vector) >= $4
      ELSE false END
ORDER BY CASE WHEN version = $2
    THEN embedding OPERATOR(public.<=>) $3::public.vector ELSE NULL END
LIMIT 20
