CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  cwd TEXT,
  title TEXT,
  model TEXT,
  parent_session_id TEXT,
  started_at REAL NOT NULL
);
CREATE TABLE messages (
  id INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL,
  role TEXT NOT NULL,
  content TEXT,
  tool_name TEXT,
  tool_calls TEXT,
  finish_reason TEXT
);
INSERT INTO sessions VALUES ('fixture-hermes-1', '/tmp/project', 'Fix auth', 'deepseek-v4.1-flash', NULL, 1789900800.0);
INSERT INTO messages VALUES (1, 'fixture-hermes-1', 'user', 'fix auth', NULL, NULL, NULL);
INSERT INTO messages VALUES (2, 'fixture-hermes-1', 'assistant', 'running tests', 'bash', '[{"name":"bash","arguments":{"command":"cargo test"}}]', 'stop');
