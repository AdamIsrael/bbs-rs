-- Mirrored board posts remember what they reply to (#139 Slice B).
--
-- Replies started syndicating in Slice A, so a subscribed board now sends us
-- `Note`s carrying `inReplyTo` — but the mirror had nowhere to put it, and every
-- reply landed as if it were a top-level post. A thread read as a flat pile in
-- arrival order, which is worse than not showing it: it silently misrepresents
-- the conversation rather than admitting it doesn't know the shape.
--
-- Stored as the parent's **URI**, not a local row id, for the same reason the
-- rest of the mirror is keyed by `ap_id`: these are foreign objects, they arrive
-- in any order, and a reply routinely arrives before the post it answers. A URI
-- resolves whenever the parent shows up (or never, which the UI handles by
-- treating the reply as a root — the same thing `boards::list_thread` does for a
-- local reply whose parent was deleted).
ALTER TABLE ap_board_posts ADD COLUMN in_reply_to TEXT;

-- Thread assembly looks up children by parent URI within a board.
CREATE INDEX idx_ap_board_posts_reply ON ap_board_posts (group_uri, in_reply_to);
