-- Conversations with the repository's AI (read-only, from the console). A conversation belongs to the
-- person who started it; its messages are kept so the next question carries what was said before, and
-- so the conversation can be turned into an issue.
CREATE TABLE conversations (
  id         bigserial PRIMARY KEY,
  service_id bigint NOT NULL REFERENCES services(id) ON DELETE CASCADE,
  owner      text NOT NULL,                       -- channel:ref of the person
  title      text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX conversations_owner ON conversations (owner, updated_at DESC);

CREATE TABLE conversation_messages (
  id              bigserial PRIMARY KEY,
  conversation_id bigint NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  role            text NOT NULL CHECK (role IN ('person', 'ai')),
  body            text NOT NULL,
  created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX conversation_messages_conv ON conversation_messages (conversation_id, id);
