-- 为 sessions 表添加 permission 列（session 级权限规则 JSON）
ALTER TABLE sessions ADD COLUMN permission TEXT;
