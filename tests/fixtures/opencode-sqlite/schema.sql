CREATE TABLE message (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  time_created INTEGER NOT NULL,
  time_updated INTEGER NOT NULL,
  data TEXT NOT NULL
);

CREATE TABLE part (
  id TEXT PRIMARY KEY,
  message_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  time_created INTEGER NOT NULL,
  time_updated INTEGER NOT NULL,
  data TEXT NOT NULL
);

INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
  (
    'msg-user-sql-001',
    'test-session-123',
    1706459830000,
    1706459830000,
    '{"role":"user","time":{"created":1706459830000},"agent":"coder","model":{"providerID":"openai","modelID":"gpt-5"}}'
  ),
  (
    'msg-assistant-sql-001',
    'test-session-123',
    1706459831000,
    1706459835000,
    '{"role":"assistant","time":{"created":1706459831000,"completed":1706459835000},"parentID":"msg-user-sql-001","modelID":"gpt-5","providerID":"openai","mode":"build","path":{"cwd":"/Users/test/project","root":"/Users/test/project"},"cost":0,"tokens":{"input":10,"output":20,"reasoning":0,"cache":{"read":0,"write":0}}}'
  );

INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
  (
    'prt-sql-001',
    'msg-user-sql-001',
    'test-session-123',
    1706459830001,
    1706459830001,
    '{"type":"text","text":"Please update index.ts using sqlite transcript data"}'
  ),
  (
    'prt-sql-002',
    'msg-assistant-sql-001',
    'test-session-123',
    1706459831001,
    1706459831001,
    '{"type":"text","text":"I will make the edit from sqlite."}'
  ),
  (
    'prt-sql-003',
    'msg-assistant-sql-001',
    'test-session-123',
    1706459832000,
    1706459833000,
    '{"type":"tool","tool":"edit","callID":"call-sql-001","state":{"status":"completed","input":{"filePath":"/Users/test/project/index.ts","content":"// sqlite path\\nconsole.log(\"sqlite\");"},"output":"Edit applied","title":"index.ts","metadata":{},"time":{"start":1706459832000,"end":1706459833000}}}'
  ),
  (
    'prt-sql-004',
    'msg-assistant-sql-001',
    'test-session-123',
    1706459834000,
    1706459834500,
    '{"type":"text","text":"Done from sqlite source."}'
  );
