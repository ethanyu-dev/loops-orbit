-- 旧资料缺少提及姓名替换和重处理姓名补全，逐日重新提取并重新生成有原话依据的摘要。
UPDATE communication_documents SET extraction_version=0,next_extraction=now();
