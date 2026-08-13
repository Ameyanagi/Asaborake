-- Asaborake's job store.
--
-- The queue lives in SQLite rather than in memory because the thing it is
-- queueing takes minutes to hours: a restart during an evening's recordings
-- must not lose what was waiting, and a job that was running when the process
-- died has to be recognisable as such rather than silently vanishing.

CREATE TABLE IF NOT EXISTS jobs (
    id              TEXT PRIMARY KEY NOT NULL,

    input           TEXT NOT NULL,
    output          TEXT NOT NULL,
    profile         TEXT NOT NULL,

    -- Recording context. Optional because a job submitted by hand has none.
    title           TEXT,
    channel_id      TEXT,
    channel_name    TEXT,

    -- queued | running | completed | failed | cancelled
    status          TEXT NOT NULL DEFAULT 'queued',
    -- Higher runs first; ties break by submission order.
    priority        INTEGER NOT NULL DEFAULT 0,

    progress        REAL NOT NULL DEFAULT 0.0,
    message         TEXT NOT NULL DEFAULT '',
    error           TEXT,

    -- The analysis and cut plan, as JSON, once the job has produced them.
    -- Stored rather than recomputed: the timeline editor reads them, and
    -- reproducing them means decoding the whole recording again.
    analysis        TEXT,
    plan            TEXT,

    created_at      TEXT NOT NULL,
    started_at      TEXT,
    finished_at     TEXT
);

-- The worker's only query: the highest-priority queued job, oldest first.
CREATE INDEX IF NOT EXISTS jobs_queue
    ON jobs (status, priority DESC, created_at);

-- The dashboard's only query: most recent first.
CREATE INDEX IF NOT EXISTS jobs_recent
    ON jobs (created_at DESC);

-- Log lines, kept per job so a failure can be explained after the fact
-- without trawling the process log.
CREATE TABLE IF NOT EXISTS job_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      TEXT NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    at          TEXT NOT NULL,
    level       TEXT NOT NULL,
    message     TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS job_events_by_job
    ON job_events (job_id, id);
