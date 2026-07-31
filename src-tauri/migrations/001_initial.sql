PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
);

CREATE TABLE clips (
  id TEXT PRIMARY KEY NOT NULL,
  content TEXT NOT NULL,
  normalized_content TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  content_type TEXT NOT NULL CHECK (content_type IN ('Text','Links','Email','Numbers')),
  domain TEXT,
  page_title TEXT,
  created_at TEXT NOT NULL,
  last_copied_at TEXT NOT NULL,
  copy_count INTEGER NOT NULL DEFAULT 1 CHECK (copy_count >= 1),
  pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
  sort_key INTEGER NOT NULL DEFAULT 0,
  UNIQUE(normalized_content, domain)
);

CREATE TABLE user_categories (
  id TEXT PRIMARY KEY NOT NULL,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  color TEXT NOT NULL,
  created_at TEXT NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE clip_user_categories (
  clip_id TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
  category_id TEXT NOT NULL REFERENCES user_categories(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY (clip_id, category_id)
);

CREATE INDEX idx_clips_recency ON clips(last_copied_at DESC);
CREATE INDEX idx_clips_type_recency ON clips(content_type, last_copied_at DESC);
CREATE INDEX idx_clips_domain_recency ON clips(domain, last_copied_at DESC);
CREATE INDEX idx_clip_categories_category ON clip_user_categories(category_id, clip_id);
