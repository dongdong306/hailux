-- 为 messages 表添加消息级模型列（一次 LLM 请求的实际模型记录在 assistant 消息行上），
-- 修复会话中途切换模型后用量统计错误归属的问题。
ALTER TABLE messages ADD COLUMN model TEXT;

-- 旧数据最佳努力回填：
-- AgentComplete 会把 {"model": ...} 写入每个任务轮次（turn）最后一条 assistant 行的
-- runtime_meta；同一 turn 内模型恒定（chat_stream 启动时快照 client/model），因此每条
-- assistant 行取同 session 内最近的后继（含自身）标记行的模型。
-- 注意 json_valid 过滤：工具调用中间行的 runtime_meta 存的是展示文本（非 JSON）。
-- 边界：进程崩溃的轮次没有标记行，其中间行会借用后继轮次的模型（无法区分轮次边界）；
-- 后面再无标记行的行（更早版本数据）保持 NULL，统计时回退 sessions.model。
UPDATE messages SET model = (
  SELECT json_extract(m2.runtime_meta, '$.model')
  FROM messages m2
  WHERE m2.session_id = messages.session_id
    AND m2.id >= messages.id
    AND json_valid(m2.runtime_meta)
    AND json_extract(m2.runtime_meta, '$.model') IS NOT NULL
  ORDER BY m2.id ASC LIMIT 1
)
WHERE role = 'assistant' AND model IS NULL;

-- 重建 usage 覆盖索引以包含 model 列（by_model / list_recent_usage 可直接从索引取值）。
DROP INDEX IF EXISTS idx_messages_usage;
CREATE INDEX idx_messages_usage ON messages(role, prompt_tokens, created_at, cached_tokens, completion_tokens, model);
