-- Schema version 3: `/ask` budgets for HTTP callers (plan sections 10.2 and 11).
-- `POST /api/v1/ask` is called with a personal access token, and access tokens are not
-- linked to a Discord member, so a usage row names either the member (Discord) or the
-- token (HTTP) it is charged to. Exactly one of the two is set on rows written by this
-- version; rows from version 2 only have `member_id`.
ALTER TABLE llm_usage ADD COLUMN token_id INTEGER REFERENCES access_tokens (id);

-- Budget checks read one asker's rows in a time window.
CREATE INDEX llm_usage_member_at ON llm_usage (member_id, at);
CREATE INDEX llm_usage_token_at ON llm_usage (token_id, at);
