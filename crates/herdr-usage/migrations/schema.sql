-- herdr-usage schema. Single owner: this file.
-- Mirrors plan §8.1; column semantics documented there.

PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS usage_event(
  source       TEXT NOT NULL,    -- omp|codex|grok|opencode
  event_id     TEXT NOT NULL,    -- dedup key, see plan §2.3
  model        TEXT,             -- providerID/modelID join string; NULL when unresolved
  model_source TEXT,             -- event|context|config_fallback|NULL
  provider     TEXT,             -- omp: message.provider；opencode: providerID；无该字段的源为 NULL
  session_id   TEXT,             -- upstream session/thread id; never conversation content
  started_at   INTEGER,          -- request dispatch time, UTC epoch ms
  completed_at INTEGER,          -- response completion time, UTC epoch ms
  duration_ms  INTEGER,          -- end-to-end request latency (completed - started)
  account_key  TEXT,             -- stable, non-secret credential identity
  account_label TEXT,            -- human-readable email or redacted key fingerprint
  account_source TEXT,           -- credential_pin|sticky_cache|single_credential|upstream
  input_total  INTEGER NOT NULL, -- gross, includes cache_read
  cache_read   INTEGER NOT NULL, -- 0 <= cache_read <= input_total
  cache_write  INTEGER NOT NULL, -- only opencode non-zero; not displayed, feeds cost
  output_total INTEGER NOT NULL, -- includes reasoning
  reasoning    INTEGER NOT NULL, -- 0 <= reasoning <= output_total; never summed
  cost_usd     REAL,             -- NULL when not priceable
  occurred_at  INTEGER NOT NULL, -- UTC epoch ms
  PRIMARY KEY(source, event_id)) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_usage_time ON usage_event(occurred_at);

-- 套餐额度页按 (provider, 时间区间) 聚合，一张索引覆盖三种套餐的过滤条件。
CREATE INDEX IF NOT EXISTS idx_usage_provider_time ON usage_event(provider, occurred_at);

-- Request/session analysis and the cross-account-session detector are covered
-- without assuming that one session belongs to only one credential.
CREATE INDEX IF NOT EXISTS idx_usage_session_time ON usage_event(session_id, started_at);
CREATE INDEX IF NOT EXISTS idx_usage_account_time ON usage_event(provider, account_key, started_at);
-- Match the recent-request ORDER BY and predicate so LIMIT can stop after 3000 rows.
CREATE INDEX IF NOT EXISTS idx_usage_recent_request
  ON usage_event(COALESCE(started_at, occurred_at) DESC)
  WHERE source IN ('omp', 'codex', 'grok', 'opencode')
    AND session_id IS NOT NULL AND session_id <> '';
-- One ordered index feeds the account rollup used by /api/requests. The
-- partial predicate excludes rows that can never participate in the report.
CREATE INDEX IF NOT EXISTS idx_usage_provider_session_account
  ON usage_event(provider, session_id, account_key, started_at, source)
  WHERE session_id IS NOT NULL AND session_id <> ''
    AND account_key IS NOT NULL AND account_key <> '';

CREATE TABLE IF NOT EXISTS collect_offset(
  kind   TEXT PRIMARY KEY,       -- omp|codex|grok|opencode，外加非采集源的水位行（omp-provider-backfill）
  cursor TEXT NOT NULL);         -- omp/codex/grok: {path: offset}; opencode: {max_rowid}

CREATE TABLE IF NOT EXISTS model_price(
  model_id    TEXT PRIMARY KEY,  -- OpenRouter id, e.g. openai/gpt-6-astra
  prompt      REAL NOT NULL,     -- USD per token
  completion  REAL NOT NULL,
  cache_read  REAL,              -- input_cache_read; NULL when absent
  cache_write REAL,              -- input_cache_write; NULL when absent
  remark      TEXT,              -- non-NULL = deviation from standard OpenRouter fields
  updated_at  INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS model_alias(
  raw_model   TEXT PRIMARY KEY,
  model_id    TEXT,              -- -> model_price.model_id; NULL when ignore=1
  ignore      INTEGER NOT NULL DEFAULT 0,
  resolved_by TEXT NOT NULL,     -- exact|bare|manual|ignore
  remark      TEXT);

CREATE TABLE IF NOT EXISTS unresolved_model(
  raw_model  TEXT NOT NULL,
  source     TEXT NOT NULL,
  hit_count  INTEGER NOT NULL DEFAULT 0,
  first_seen INTEGER NOT NULL,   -- UTC epoch ms
  last_seen  INTEGER NOT NULL,   -- UTC epoch ms; window predicate is on this column
  PRIMARY KEY(source, raw_model));

CREATE INDEX IF NOT EXISTS idx_unresolved_recent ON unresolved_model(last_seen);
