-- Per-user opt-out from the finger service (#77). NULL/0 = listed (the
-- default), 1 = the user has hidden themselves from `finger user@host` and the
-- who's-online listing. Additive; existing users default to listed.
ALTER TABLE users ADD COLUMN finger_optout INTEGER NOT NULL DEFAULT 0;
