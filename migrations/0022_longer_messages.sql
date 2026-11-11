-- Messages to the agent may be longer: a conversation with the AI can be sent along as it is.
ALTER TABLE ticket_messages DROP CONSTRAINT ticket_messages_body_check;
ALTER TABLE ticket_messages ADD CONSTRAINT ticket_messages_body_check CHECK (char_length(body) BETWEEN 1 AND 10000);
