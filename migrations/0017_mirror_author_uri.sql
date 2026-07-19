-- Remote lifecycle needs an owner to authorize against (epic #113, #112 Slice B).
--
-- `ap_board_posts` stored only the author's display handle, which isn't an
-- identity — a `Delete`/`Update` has to be checked against the author's actor
-- URI (or the announcing board's). Add it so mirrored posts can be authorized
-- like every other federated store.
--
-- Nullable: rows mirrored before this migration keep working and are simply
-- only deletable by the board that announced them.
ALTER TABLE ap_board_posts ADD COLUMN author_uri TEXT;
