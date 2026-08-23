-- 为 messages 表添加缓存命中 token 列（一次 LLM 请求的 usage 记录在 assistant 消息行上）
ALTER TABLE messages ADD COLUMN cached_tokens INTEGER;

-- 用量统计查询覆盖索引（usage_summary/daily/by_model/by_project/list_recent_usage）：
-- WHERE role='assistant' AND prompt_tokens IS NOT NULL AND created_at >= ?，
-- 且 SUM/COUNT 的列均可从索引直接取值，无需回表。
CREATE INDEX IF NOT EXISTS idx_messages_usage ON messages(role, prompt_tokens, created_at, cached_tokens, completion_tokens);
