-- Set by a dance a signed-in session started, so the callback attaches the
-- identity to that user. NULL is a plain login, which refuses to link — so a
-- callback that ignores this column fails closed.
ALTER TABLE oauth_states
    ADD COLUMN link_user_id UUID REFERENCES users(id) ON DELETE CASCADE;
