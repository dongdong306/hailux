-- 图片附件：以 JSON 数组存储 [{mime, data_url}]（data:<mime>;base64,<data>）。
-- 仅用户消息使用；合成 user 消息（工具结果剥离的媒体）同列存储，保证历史重放可用。
ALTER TABLE messages ADD COLUMN attachments TEXT;
