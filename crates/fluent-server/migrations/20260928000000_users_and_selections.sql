-- Hosted users and the registrations each one selected. Times are Unix
-- seconds.
CREATE TABLE users (
    sub TEXT PRIMARY KEY NOT NULL,
    email TEXT,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE TABLE selections (
    sub TEXT NOT NULL REFERENCES users (sub) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    -- The registration's revision when it was selected.
    revision TEXT NOT NULL,
    selected_at INTEGER NOT NULL,
    PRIMARY KEY (sub, slug)
);
