-- Messages a person with approval rights sends to the agent working on a ticket. They are given to the
-- next run on the ticket (triage, development or review) as a team message, then marked delivered.
CREATE TABLE ticket_messages (
  id           bigserial PRIMARY KEY,
  ticket_id    bigint NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
  author       text NOT NULL,                       -- channel:ref of the person
  body         text NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
  created_at   timestamptz NOT NULL DEFAULT now(),
  delivered_at timestamptz
);
CREATE INDEX ticket_messages_pending ON ticket_messages (ticket_id) WHERE delivered_at IS NULL;
