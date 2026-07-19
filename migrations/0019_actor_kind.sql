-- Remember what kind of actor a remote row is (epic #113, #132).
--
-- Remote actors are stored as `users` rows regardless of type — a `Person` we
-- follow and a `Group` (a remote board) look identical in the schema. That was
-- fine while both were only ever reached through their actor URI, but the
-- in-BBS screen for mirrored boards needs to *list* subscribed boards, and
-- "which of these follows are boards?" has no answer today.
--
-- Nullable rather than defaulted: 'Person' would be a guess about rows we never
-- recorded a type for, and a wrong guess here silently hides a board from its
-- own screen. NULL honestly means "we don't know yet"; the value is filled in
-- the next time the actor is fetched.
ALTER TABLE users ADD COLUMN actor_kind TEXT;

-- Backfill what we can prove rather than what we assume: any actor that has
-- announced a board post to us is a Group, on the evidence of the post.
UPDATE users
   SET actor_kind = 'Group'
 WHERE actor_uri IN (SELECT DISTINCT group_uri FROM ap_board_posts);

CREATE INDEX idx_users_actor_kind ON users (actor_kind) WHERE actor_kind IS NOT NULL;
