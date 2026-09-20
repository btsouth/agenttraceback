CREATE TABLE session (
  id TEXT PRIMARY KEY,
  directory TEXT NOT NULL,
  title TEXT NOT NULL,
  model TEXT,
  project_id TEXT NOT NULL
);
CREATE TABLE message (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  time_created INTEGER NOT NULL,
  data TEXT NOT NULL
);
CREATE TABLE part (
  id TEXT PRIMARY KEY,
  message_id TEXT NOT NULL,
  data TEXT NOT NULL,
  time_created INTEGER NOT NULL
);
INSERT INTO session VALUES ('fixture-opencode-1', '/tmp/project', 'Add tests', 'anthropic/claude-sonnet', 'fixture-project');
INSERT INTO message VALUES ('m1', 'fixture-opencode-1', 1789900800000, '{"role":"user"}');
INSERT INTO part VALUES ('p1', 'm1', '{"type":"text","text":"add tests"}', 1789900800001);
INSERT INTO message VALUES ('m2', 'fixture-opencode-1', 1789900801000, '{"role":"assistant"}');
INSERT INTO part VALUES ('p2', 'm2', '{"type":"tool","tool":"bash","state":{"input":{"command":"cargo test"}}}', 1789900801001);
