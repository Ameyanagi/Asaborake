//! The job store.
//!
//! Every query here is written at runtime rather than with sqlx's compile-time
//! macros, so building Asaborake needs no database present. The schema is
//! small enough that the macros' checking would not repay that cost.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row as _, SqlitePool};

use crate::Error;

/// Where a job has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting for a worker.
    Queued,
    /// Being analysed or encoded.
    Running,
    /// Finished successfully.
    Completed,
    /// Finished unsuccessfully.
    Failed,
    /// Stopped on request.
    Cancelled,
}

impl JobStatus {
    /// The string stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the stored form, defaulting to `Failed` for anything unknown.
    ///
    /// An unrecognised status means the row was written by another version;
    /// treating it as failed keeps it out of the queue rather than having a
    /// worker pick up something it cannot interpret. Infallible by design,
    /// which is why this is not `FromStr`.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }

    /// Whether the job has stopped, one way or another.
    #[must_use]
    pub const fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// One job, as the API presents it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Identifier, assigned on submission.
    pub id: String,
    /// Source recording.
    pub input: String,
    /// Where the result goes.
    pub output: String,
    /// Encoding profile name.
    pub profile: String,
    /// Programme title, when known.
    pub title: Option<String>,
    /// Channel id, when known.
    pub channel_id: Option<String>,
    /// Channel name, when known.
    pub channel_name: Option<String>,
    /// Where the job has got to.
    pub status: JobStatus,
    /// Higher runs first.
    pub priority: i64,
    /// Completion, in `0.0..=1.0`.
    pub progress: f64,
    /// What is happening right now.
    pub message: String,
    /// Why it failed, if it did.
    pub error: Option<String>,
    /// When it was submitted.
    pub created_at: DateTime<Utc>,
    /// When a worker picked it up.
    pub started_at: Option<DateTime<Utc>>,
    /// When it stopped.
    pub finished_at: Option<DateTime<Utc>>,
}

/// A request to queue a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewJob {
    /// Source recording.
    pub input: String,
    /// Where the result goes.
    pub output: String,
    /// Encoding profile name.
    pub profile: String,
    /// Programme title.
    #[serde(default)]
    pub title: Option<String>,
    /// Channel id, which keys the logo store.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// Channel name.
    #[serde(default)]
    pub channel_name: Option<String>,
    /// Higher runs first.
    #[serde(default)]
    pub priority: i64,
}

/// One logged line from a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEvent {
    /// Monotonic id, for resuming a log tail.
    pub id: i64,
    /// When it happened.
    pub at: DateTime<Utc>,
    /// `info`, `warn` or `error`.
    pub level: String,
    /// The line itself.
    pub message: String,
}

/// A handle to the job store.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open the store at `path`, creating and migrating it as needed.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the file cannot be opened or migrated.
    pub async fn open(path: &std::path::Path) -> Result<Self, Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // Write-ahead logging lets the API read while a worker writes,
            // which is the whole access pattern here.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(10))
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .map_err(Error::Database)?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|error| Error::Migrate(Box::new(error)))?;

        Ok(Self { pool })
    }

    /// Queue a job, returning its id.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the insert fails.
    pub async fn submit(&self, request: &NewJob) -> Result<String, Error> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO jobs (id, input, output, profile, title, channel_id, channel_name,
                               status, priority, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
        )
        .bind(&id)
        .bind(&request.input)
        .bind(&request.output)
        .bind(&request.profile)
        .bind(request.title.as_deref())
        .bind(request.channel_id.as_deref())
        .bind(request.channel_name.as_deref())
        .bind(request.priority)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(id)
    }

    /// Claim the next job for a worker, marking it running.
    ///
    /// The claim is a conditional update rather than a select followed by an
    /// update, so two workers racing for the last queued job cannot both win:
    /// only one `UPDATE ... WHERE status = 'queued'` affects a row.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the query fails.
    pub async fn claim_next(&self) -> Result<Option<Job>, Error> {
        let claimed = sqlx::query(
            "UPDATE jobs SET status = 'running', started_at = ?
             WHERE id = (
                 SELECT id FROM jobs WHERE status = 'queued'
                 ORDER BY priority DESC, created_at
                 LIMIT 1
             )
             RETURNING *",
        )
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(claimed.as_ref().map(row_to_job))
    }

    /// Fetch one job.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the query fails.
    pub async fn get(&self, id: &str) -> Result<Option<Job>, Error> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(row.as_ref().map(row_to_job))
    }

    /// List jobs, most recent first.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the query fails.
    pub async fn list(&self, limit: i64) -> Result<Vec<Job>, Error> {
        let rows = sqlx::query("SELECT * FROM jobs ORDER BY created_at DESC LIMIT ?")
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(rows.iter().map(row_to_job).collect())
    }

    /// Record progress against a running job.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the update fails.
    pub async fn set_progress(&self, id: &str, progress: f64, message: &str) -> Result<(), Error> {
        sqlx::query("UPDATE jobs SET progress = ?, message = ? WHERE id = ?")
            .bind(progress.clamp(0.0, 1.0))
            .bind(message)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(())
    }

    /// Record what the source recording contained, as JSON.
    ///
    /// Written when the job starts rather than when it finishes, so a job that
    /// fails still says what it was working from — which is the case where a
    /// damaged recording explains the failure.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the update fails.
    pub async fn set_diagnostics(&self, id: &str, diagnostics: &str) -> Result<(), Error> {
        sqlx::query("UPDATE jobs SET diagnostics = ? WHERE id = ?")
            .bind(diagnostics)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(())
    }

    /// Mark a job finished, with its outcome.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the update fails.
    pub async fn finish(
        &self,
        id: &str,
        status: JobStatus,
        error: Option<&str>,
        analysis: Option<&str>,
        plan: Option<&str>,
    ) -> Result<(), Error> {
        // Deliberately does not touch `diagnostics`: that is written by
        // [`set_diagnostics`](Self::set_diagnostics) when the job starts, and
        // clearing it here would lose it on exactly the failed jobs where it
        // explains what happened.
        sqlx::query(
            "UPDATE jobs
             SET status = ?, error = ?, analysis = ?, plan = ?,
                 finished_at = ?, progress = CASE WHEN ? = 'completed' THEN 1.0 ELSE progress END
             WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(error)
        .bind(analysis)
        .bind(plan)
        .bind(Utc::now().to_rfc3339())
        .bind(status.as_str())
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(())
    }

    /// Ask for a job to stop, or drop it from the queue if it has not started.
    ///
    /// Returns whether anything changed.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the update fails.
    pub async fn cancel(&self, id: &str) -> Result<bool, Error> {
        let result = sqlx::query(
            "UPDATE jobs SET status = 'cancelled', finished_at = ?
             WHERE id = ? AND status IN ('queued', 'running')",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() > 0)
    }

    /// Whether a job has been asked to stop.
    ///
    /// Polled by the worker between phases, since a job in the middle of an
    /// ffmpeg run cannot be interrupted at an arbitrary point.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the query fails.
    pub async fn is_cancelled(&self, id: &str) -> Result<bool, Error> {
        let row = sqlx::query("SELECT status FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(row.is_some_and(|row| {
            row.try_get::<String, _>("status")
                .is_ok_and(|status| status == "cancelled")
        }))
    }

    /// Put a job that was running back in the queue.
    ///
    /// Called at startup: a job marked running when the process starts was
    /// interrupted by a restart, since nothing else could have left it that
    /// way. Requeuing is safe because the pipeline writes its output only at
    /// the end.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the update fails.
    pub async fn requeue_interrupted(&self) -> Result<u64, Error> {
        let result = sqlx::query(
            "UPDATE jobs SET status = 'queued', started_at = NULL, progress = 0.0,
                             message = 'requeued after restart'
             WHERE status = 'running'",
        )
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected())
    }

    /// Append a log line to a job.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the insert fails.
    pub async fn log(&self, id: &str, level: &str, message: &str) -> Result<(), Error> {
        sqlx::query("INSERT INTO job_events (job_id, at, level, message) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(Utc::now().to_rfc3339())
            .bind(level)
            .bind(message)
            .execute(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(())
    }

    /// Read a job's log, from `after` onwards.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the query fails.
    pub async fn events(&self, id: &str, after: i64) -> Result<Vec<JobEvent>, Error> {
        let rows = sqlx::query(
            "SELECT id, at, level, message FROM job_events
             WHERE job_id = ? AND id > ? ORDER BY id LIMIT 1000",
        )
        .bind(id)
        .bind(after)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(rows
            .iter()
            .map(|row| JobEvent {
                id: row.try_get("id").unwrap_or_default(),
                at: parse_time(row.try_get("at").ok()).unwrap_or_else(Utc::now),
                level: row.try_get("level").unwrap_or_default(),
                message: row.try_get("message").unwrap_or_default(),
            })
            .collect())
    }

    /// Everything a job produced besides the file itself, as raw JSON.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the query fails.
    pub async fn artifacts(&self, id: &str) -> Result<Option<Artifacts>, Error> {
        let row = sqlx::query("SELECT analysis, plan, diagnostics FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(row.map(|row| Artifacts {
            analysis: row.try_get("analysis").ok(),
            plan: row.try_get("plan").ok(),
            diagnostics: row.try_get("diagnostics").ok(),
        }))
    }
}

/// What a finished job left behind, each as the JSON it was stored as.
///
/// Held as text rather than parsed because the server only forwards these to
/// the browser; parsing them here would cost a round trip through serde and
/// risk rejecting a document an older version wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Artifacts {
    /// What analysis found.
    pub analysis: Option<String>,
    /// What was decided.
    pub plan: Option<String>,
    /// What the recording contained and what was wrong with it.
    pub diagnostics: Option<String>,
}

/// Build a [`Job`] from a row, defaulting anything unreadable.
///
/// A row written by a future version must not take the whole listing down, so
/// individual columns fall back rather than propagating an error.
fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> Job {
    Job {
        id: row.try_get("id").unwrap_or_default(),
        input: row.try_get("input").unwrap_or_default(),
        output: row.try_get("output").unwrap_or_default(),
        profile: row.try_get("profile").unwrap_or_default(),
        title: optional(row, "title"),
        channel_id: optional(row, "channel_id"),
        channel_name: optional(row, "channel_name"),
        status: JobStatus::parse(&row.try_get::<String, _>("status").unwrap_or_default()),
        priority: row.try_get("priority").unwrap_or(0),
        progress: row.try_get("progress").unwrap_or(0.0),
        message: row.try_get("message").unwrap_or_default(),
        error: optional(row, "error"),
        created_at: parse_time(row.try_get("created_at").ok()).unwrap_or_else(Utc::now),
        started_at: parse_time(row.try_get("started_at").ok()),
        finished_at: parse_time(row.try_get("finished_at").ok()),
    }
}

/// Read a nullable text column, treating an empty string as absent.
///
/// The two are the same thing to every caller — an unknown title is an unknown
/// title — and collapsing them here means the API never has to present both.
fn optional(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

/// Parse an RFC 3339 timestamp from the database.
fn parse_time(value: Option<String>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("jobs.db"))
            .await
            .expect("store opens");
        (store, dir)
    }

    fn job(input: &str) -> NewJob {
        NewJob {
            input: input.to_owned(),
            output: format!("{input}.mp4"),
            profile: "x264-cpu".to_owned(),
            title: Some("Test".to_owned()),
            channel_id: Some("1024".to_owned()),
            channel_name: Some("ABC".to_owned()),
            priority: 0,
        }
    }

    #[tokio::test]
    async fn a_submitted_job_can_be_read_back() {
        let (store, _dir) = store().await;
        let id = store.submit(&job("a.ts")).await.expect("submits");

        let found = store.get(&id).await.expect("queries").expect("exists");
        assert_eq!(found.status, JobStatus::Queued);
        assert_eq!(found.input, "a.ts");
        assert_eq!(found.channel_id.as_deref(), Some("1024"));
    }

    #[tokio::test]
    async fn claiming_takes_the_highest_priority_job_first() {
        let (store, _dir) = store().await;
        store.submit(&job("low.ts")).await.expect("submits");
        let urgent = NewJob {
            priority: 10,
            ..job("urgent.ts")
        };
        store.submit(&urgent).await.expect("submits");

        let claimed = store.claim_next().await.expect("claims").expect("a job");
        assert_eq!(claimed.input, "urgent.ts");
        assert_eq!(claimed.status, JobStatus::Running);
    }

    #[tokio::test]
    async fn a_job_is_only_claimed_once() {
        let (store, _dir) = store().await;
        store.submit(&job("only.ts")).await.expect("submits");

        assert!(store.claim_next().await.expect("claims").is_some());
        assert!(
            store.claim_next().await.expect("claims").is_none(),
            "a second worker must not claim the same job"
        );
    }

    #[tokio::test]
    async fn an_interrupted_job_returns_to_the_queue() {
        let (store, _dir) = store().await;
        store.submit(&job("interrupted.ts")).await.expect("submits");
        let claimed = store.claim_next().await.expect("claims").expect("a job");

        // Nothing but a restart can leave a job marked running at startup.
        let requeued = store.requeue_interrupted().await.expect("requeues");
        assert_eq!(requeued, 1);

        let found = store
            .get(&claimed.id)
            .await
            .expect("queries")
            .expect("exists");
        assert_eq!(found.status, JobStatus::Queued);
        assert!(store.claim_next().await.expect("claims").is_some());
    }

    #[tokio::test]
    async fn cancelling_only_affects_a_job_that_has_not_finished() {
        let (store, _dir) = store().await;
        let id = store.submit(&job("c.ts")).await.expect("submits");

        assert!(store.cancel(&id).await.expect("cancels"));
        assert!(store.is_cancelled(&id).await.expect("queries"));
        // Cancelling again changes nothing.
        assert!(!store.cancel(&id).await.expect("cancels"));
        // And a cancelled job is not queued.
        assert!(store.claim_next().await.expect("claims").is_none());
    }

    #[tokio::test]
    async fn finishing_records_the_outcome_and_completes_the_bar() {
        let (store, _dir) = store().await;
        let id = store.submit(&job("f.ts")).await.expect("submits");
        store.claim_next().await.expect("claims");
        store
            .set_progress(&id, 0.5, "encoding")
            .await
            .expect("progresses");

        store
            .set_diagnostics(&id, r#"{"warnings":["reception was poor"]}"#)
            .await
            .expect("records the source");
        store
            .finish(&id, JobStatus::Completed, None, Some("{}"), Some("{}"))
            .await
            .expect("finishes");

        let found = store.get(&id).await.expect("queries").expect("exists");
        assert_eq!(found.status, JobStatus::Completed);
        assert!((found.progress - 1.0).abs() < 1e-9, "{}", found.progress);
        assert!(found.finished_at.is_some());

        let artifacts = store
            .artifacts(&id)
            .await
            .expect("queries")
            .expect("exists");
        assert_eq!(artifacts.analysis.as_deref(), Some("{}"));
        // Finishing must not clear what was recorded when the job started.
        assert!(
            artifacts
                .diagnostics
                .as_deref()
                .is_some_and(|d| d.contains("reception was poor")),
            "{:?}",
            artifacts.diagnostics
        );
    }

    #[tokio::test]
    async fn a_failure_keeps_its_progress_and_records_why() {
        let (store, _dir) = store().await;
        let id = store.submit(&job("bad.ts")).await.expect("submits");
        store.claim_next().await.expect("claims");
        store.set_progress(&id, 0.3, "encoding").await.expect("ok");

        store
            .finish(&id, JobStatus::Failed, Some("ffmpeg exited 1"), None, None)
            .await
            .expect("finishes");

        let found = store.get(&id).await.expect("queries").expect("exists");
        assert_eq!(found.status, JobStatus::Failed);
        assert!((found.progress - 0.3).abs() < 1e-9, "progress must be kept");
        assert_eq!(found.error.as_deref(), Some("ffmpeg exited 1"));
    }

    #[tokio::test]
    async fn a_failed_job_keeps_what_its_source_was() {
        // A recording damaged enough to fail analysis is the one whose
        // diagnostics explain the failure, so they must survive it.
        let (store, _dir) = store().await;
        let id = store.submit(&job("broken.ts")).await.expect("submits");
        store.claim_next().await.expect("claims");
        store
            .set_diagnostics(&id, r#"{"scrambled_packets":900000}"#)
            .await
            .expect("records the source");

        store
            .finish(&id, JobStatus::Failed, Some("analysis error"), None, None)
            .await
            .expect("finishes");

        let artifacts = store
            .artifacts(&id)
            .await
            .expect("queries")
            .expect("exists");
        assert_eq!(
            artifacts.diagnostics.as_deref(),
            Some(r#"{"scrambled_packets":900000}"#)
        );
    }

    #[tokio::test]
    async fn events_can_be_tailed_from_a_cursor() {
        let (store, _dir) = store().await;
        let id = store.submit(&job("log.ts")).await.expect("submits");
        for line in ["one", "two", "three"] {
            store.log(&id, "info", line).await.expect("logs");
        }

        let all = store.events(&id, 0).await.expect("reads");
        assert_eq!(all.len(), 3);

        let tail = store
            .events(&id, all[0].id)
            .await
            .expect("reads from a cursor");
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].message, "two");
    }

    #[tokio::test]
    async fn an_omitted_field_reads_back_as_absent_not_empty() {
        let (store, _dir) = store().await;
        let bare = NewJob {
            title: None,
            channel_id: None,
            channel_name: None,
            ..job("bare.ts")
        };
        let id = store.submit(&bare).await.expect("submits");

        let found = store.get(&id).await.expect("queries").expect("exists");
        assert_eq!(found.title, None, "an absent title must not become empty");
        assert_eq!(found.channel_id, None);
        assert_eq!(found.error, None);
    }

    #[tokio::test]
    async fn an_unknown_status_is_treated_as_failed_rather_than_queued() {
        // A row written by a future version must not be picked up by a worker
        // that cannot interpret it.
        assert_eq!(JobStatus::parse("something-new"), JobStatus::Failed);
        assert!(JobStatus::parse("something-new").is_finished());
    }
}
