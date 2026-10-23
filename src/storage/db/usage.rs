use chrono::Local;
use color_eyre::Result;

use super::ChatStorage;
use super::models::{DailyUsage, ModelUsage, ProjectUsage, UsageRecord, UsageSummary};
use super::work_dir_variants;

impl ChatStorage {
    // ── 用量统计（基于 messages 表的 assistant usage 行）────────

    /// 统计的时间下限（ISO 本地时间）；days = 0 表示不限。
    fn usage_cutoff_iso(days: u32) -> String {
        if days == 0 {
            String::new()
        } else {
            (Local::now() - chrono::Duration::days(days as i64))
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string()
        }
    }

    /// 用量统计的公共条件绑定参数（条件片段已内联在各查询的静态 SQL 中，
    /// 共 5 个占位符：NULL 检查 1 + IN 2 + 空串检查 1 + 日期下限 1）。
    /// work_dir = None 时首三参绑 NULL（`? IS NULL` 恒真，跳过目录过滤）；
    /// days = 0 时日期下限绑空串（`? = ''` 恒真，跳过时间过滤）。
    fn usage_filter_binds(work_dir: Option<&str>, days: u32) -> [Option<String>; 5] {
        let variants = work_dir.map(work_dir_variants);
        let cutoff = Self::usage_cutoff_iso(days);
        [
            variants.as_ref().map(|v| v[0].clone()),
            variants.as_ref().map(|v| v[0].clone()),
            variants.as_ref().map(|v| v[1].clone()),
            Some(cutoff.clone()),
            Some(cutoff),
        ]
    }

    /// 汇总：请求数 / 输入 / 缓存命中 / 输出 token 累计。
    pub async fn usage_summary(&self, work_dir: Option<&str>, days: u32) -> Result<UsageSummary> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "SELECT COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?)",
        );
        for b in binds {
            q = q.bind(b);
        }
        let (requests, prompt_tokens, cached_tokens, completion_tokens) =
            q.fetch_one(&self.pool).await?;
        Ok(UsageSummary {
            requests,
            prompt_tokens,
            cached_tokens,
            completion_tokens,
        })
    }

    /// 按日聚合（本地时区日期），date 升序。
    pub async fn usage_daily(&self, work_dir: Option<&str>, days: u32) -> Result<Vec<DailyUsage>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64, i64)>(
            "SELECT substr(m.created_at, 1, 10) AS day, COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             GROUP BY day ORDER BY day ASC",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(date, requests, prompt_tokens, cached_tokens, completion_tokens)| DailyUsage {
                    date,
                    requests,
                    prompt_tokens,
                    cached_tokens,
                    completion_tokens,
                },
            )
            .collect())
    }

    /// 按模型聚合（消息级 model，旧行回退 sessions.model），token 降序。
    pub async fn usage_by_model(
        &self,
        work_dir: Option<&str>,
        days: u32,
    ) -> Result<Vec<ModelUsage>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64, i64)>(
            "SELECT COALESCE(m.model, s.model), COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             GROUP BY COALESCE(m.model, s.model) ORDER BY SUM(m.prompt_tokens) + SUM(m.completion_tokens) DESC",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(model, requests, prompt_tokens, cached_tokens, completion_tokens)| ModelUsage {
                    model,
                    requests,
                    prompt_tokens,
                    cached_tokens,
                    completion_tokens,
                },
            )
            .collect())
    }

    /// 按项目聚合（sessions.work_dir），token 降序，最多 limit 条。
    pub async fn usage_by_project(
        &self,
        work_dir: Option<&str>,
        days: u32,
        limit: u32,
    ) -> Result<Vec<ProjectUsage>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64, i64)>(
            "SELECT s.work_dir, COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             GROUP BY s.work_dir ORDER BY SUM(m.prompt_tokens) + SUM(m.completion_tokens) DESC \
             LIMIT ?",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(work_dir, requests, prompt_tokens, cached_tokens, completion_tokens)| {
                    ProjectUsage {
                        work_dir,
                        requests,
                        prompt_tokens,
                        cached_tokens,
                        completion_tokens,
                    }
                },
            )
            .collect())
    }

    /// 最近请求明细（id 降序，limit 条）。
    pub async fn list_recent_usage(
        &self,
        work_dir: Option<&str>,
        days: u32,
        limit: u32,
    ) -> Result<Vec<UsageRecord>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, String, String, i64, i64, i64)>(
            "SELECT m.created_at, COALESCE(m.model, s.model), s.work_dir, m.prompt_tokens, COALESCE(m.cached_tokens,0), m.completion_tokens \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             ORDER BY m.id DESC LIMIT ?",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(created_at, model, work_dir, prompt_tokens, cached_tokens, completion_tokens)| {
                    UsageRecord {
                        created_at,
                        model,
                        work_dir,
                        prompt_tokens,
                        cached_tokens,
                        completion_tokens,
                    }
                },
            )
            .collect())
    }
}
