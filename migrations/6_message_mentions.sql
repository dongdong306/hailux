-- 消息提及元数据（JSON：[{"start":0,"end":16,"kind":"file"}]，
-- start/end 为消息 content 中的 Unicode 码点偏移）。
ALTER TABLE messages ADD COLUMN mentions TEXT;
