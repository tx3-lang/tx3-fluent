-- Prepared-transaction requests per user and UTC day, for the daily quota.
-- `day` counts days since the Unix epoch. Rows of past days are deleted as the
-- user's next request is counted.
CREATE TABLE quota_usage (
    sub TEXT NOT NULL,
    day INTEGER NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (sub, day)
);
