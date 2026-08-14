//! The worker pool.
//!
//! A job is minutes to hours of CPU and GPU work driving child processes, so
//! each one runs on a blocking thread rather than on the async runtime. The
//! async side does nothing but hand out work, record progress, and publish
//! updates; that keeps the API responsive while several encodes are in flight.

use std::sync::Arc;
use std::time::Duration;

use asaborake_core::{JobRequest, LogoStore};
use asaborake_media::Ffmpeg;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::db::{Job, JobStatus, Store};
use crate::{Config, Error};

/// How often an idle worker looks for something to do.
///
/// Submissions also wake the pool directly, so this only bounds how long a job
/// can sit unnoticed if that notification is missed.
const IDLE_POLL: Duration = Duration::from_secs(2);

/// Something worth telling connected clients about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Update {
    /// A job's progress or status changed.
    Job {
        /// The job, in full.
        job: Box<Job>,
    },
    /// A job logged a line.
    Log {
        /// Which job.
        job_id: String,
        /// The line.
        message: String,
    },
}

/// Publishes updates to everything watching.
#[derive(Debug, Clone)]
pub struct Events {
    sender: broadcast::Sender<Update>,
}

impl Events {
    /// Create a channel with room for a reasonable backlog.
    #[must_use]
    pub fn new() -> Self {
        // A slow client falls behind and is told so, rather than holding the
        // worker up: progress is a stream of snapshots, and missing some is
        // harmless because the next one supersedes them.
        let (sender, _) = broadcast::channel(512);
        Self { sender }
    }

    /// Subscribe to the stream.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.sender.subscribe()
    }

    /// Publish an update, ignoring the absence of listeners.
    pub fn publish(&self, update: Update) {
        let _ = self.sender.send(update);
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything a worker needs.
#[derive(Clone)]
pub struct Context {
    /// The job store.
    pub store: Store,
    /// The update channel.
    pub events: Events,
    /// The ffmpeg installation.
    pub ffmpeg: Arc<Ffmpeg>,
    /// The logo store, when one is configured.
    pub logos: Option<Arc<LogoStore>>,
    /// Server configuration.
    pub config: Arc<Config>,
    /// Signals workers to look for work now rather than at the next poll.
    pub wake: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Start `count` workers, each running until cancelled.
#[must_use]
pub fn spawn_pool(context: &Context, count: usize) -> Vec<tokio::task::JoinHandle<()>> {
    (0..count.max(1))
        .map(|index| {
            let context = context.clone();
            tokio::spawn(async move {
                tracing::debug!(worker = index, "worker started");
                run_worker(context).await;
            })
        })
        .collect()
}

/// Take jobs until the process ends.
async fn run_worker(context: Context) {
    let mut resting = false;
    loop {
        // Outside its hours the queue does not stop accepting work, it stops
        // starting it. Anything already running is left alone: killing an
        // encode at 07:00 wastes the six hours it has already had.
        if !context.config.run_hours.allows_now() {
            if !resting {
                tracing::info!(
                    hours = %context.config.run_hours.describe(),
                    "outside the hours jobs may run; waiting"
                );
                resting = true;
            }
            tokio::time::sleep(IDLE_POLL).await;
            continue;
        }
        resting = false;

        match context.store.claim_next().await {
            Ok(Some(job)) => {
                let id = job.id.clone();
                if let Err(error) = run_job(&context, job).await {
                    tracing::error!(job = %id, %error, "job failed to run");
                }
            }
            Ok(None) => {
                // Nothing waiting. Sleep until something is submitted, or
                // until the poll expires in case that signal was missed.
                tokio::select! {
                    () = context.wake.notified() => {}
                    () = tokio::time::sleep(IDLE_POLL) => {}
                }
            }
            Err(error) => {
                tracing::error!(%error, "could not claim a job");
                tokio::time::sleep(IDLE_POLL).await;
            }
        }
    }
}

/// Run one job to completion.
async fn run_job(context: &Context, job: Job) -> Result<(), Error> {
    tracing::info!(job = %job.id, input = %job.input, "starting");
    context.events.publish(Update::Job {
        job: Box::new(job.clone()),
    });
    log(context, &job.id, "info", "starting").await;

    // A channel may override what the job asked for. NHK carries no
    // advertising, so looking for commercials in it spends a pass to find
    // nothing; a film channel may want a better profile than the default.
    let settings = settings_for(context, &job).await;

    let wanted = settings
        .profile
        .clone()
        .unwrap_or_else(|| job.profile.clone());
    let Some(profile) = asaborake_core::profile::ProfileStore::open(&context.config.profile_dir)
        .all()
        .remove(&wanted)
    else {
        let message = format!("no profile named '{wanted}'");
        fail(context, &job, &message).await;
        return Ok(());
    };
    if wanted != job.profile {
        log(
            context,
            &job.id,
            "info",
            &format!(
                "using '{wanted}' for this channel instead of '{}'",
                job.profile
            ),
        )
        .await;
    }

    let job = rename_output(context, job).await;

    if let Some(message) = no_room_for(&job) {
        fail(context, &job, &message).await;
        return Ok(());
    }

    let mut request = JobRequest::new(&job.input, &job.output, profile);
    request.channel_id.clone_from(&job.channel_id);
    request.channel_name.clone_from(&job.channel_name);
    request.title.clone_from(&job.title);
    request.cut.low_confidence = context.config.on_low_confidence;
    if !settings.detect_commercials {
        // Nothing to find, so nothing is looked for: no logo pass, no
        // segmentation, and the recording is transcoded whole.
        request.learn_logo = false;
        request.cut.detect = false;
        log(
            context,
            &job.id,
            "info",
            "this channel carries no commercials, so none are looked for",
        )
        .await;
    }
    request.diagnostics = inspect_source(context, &job).await;

    // Progress arrives from a blocking thread and has to cross back to the
    // async side to be recorded and published.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<(f64, String)>(64);

    let ffmpeg = Arc::clone(&context.ffmpeg);
    let logos = context.logos.clone();
    let worker = tokio::task::spawn_blocking(move || {
        asaborake_core::run(&ffmpeg, logos.as_deref(), &request, &mut |progress| {
            // A full channel means the recorder is behind; dropping the update
            // is correct, because the next one supersedes it and the encode
            // must not be held up by bookkeeping.
            let _ = progress_tx.try_send((progress.fraction, progress.message.clone()));
        })
    });

    let recorder = {
        let context = context.clone();
        let id = job.id.clone();
        tokio::spawn(async move {
            let mut last_message = String::new();
            while let Some((fraction, message)) = progress_rx.recv().await {
                let _ = context.store.set_progress(&id, fraction, &message).await;
                if message != last_message {
                    log(&context, &id, "info", &message).await;
                    last_message = message.clone();
                }
                if let Ok(Some(job)) = context.store.get(&id).await {
                    context.events.publish(Update::Job { job: Box::new(job) });
                }
            }
        })
    };

    let outcome = worker.await;
    let _ = recorder.await;

    // A cancellation that arrived mid-run is honoured even if the pipeline
    // finished, because the output is not what the operator asked for.
    if context.store.is_cancelled(&job.id).await.unwrap_or(false) {
        log(context, &job.id, "warn", "cancelled").await;
        publish(context, &job.id).await;
        return Ok(());
    }

    record(context, &job, outcome).await?;

    publish(context, &job.id).await;
    Ok(())
}

/// Record what became of a job.
///
/// Split out because the outcomes are the interesting part and were buried at
/// the end of a function that mostly sets a job up.
async fn record(
    context: &Context,
    job: &Job,
    outcome: Result<
        Result<asaborake_core::JobOutcome, asaborake_core::Error>,
        tokio::task::JoinError,
    >,
) -> Result<(), Error> {
    match outcome {
        Ok(Ok(result)) => {
            let analysis = serde_json::to_string(&result.analysis).ok();
            let plan = serde_json::to_string(&result.plan).ok();
            context
                .store
                .finish(
                    &job.id,
                    JobStatus::Completed,
                    None,
                    analysis.as_deref(),
                    plan.as_deref(),
                )
                .await?;
            log(
                context,
                &job.id,
                "info",
                // What was labelled commercial and what was actually removed
                // are different numbers whenever confidence was too low to
                // cut, and saying "removed" for the first is simply wrong.
                &if result.plan.decision == asaborake_cmcut::Decision::Cut {
                    format!(
                        "done: removed {:.1}s, confidence {:.2}",
                        result.plan.removed_seconds(),
                        result.plan.confidence
                    )
                } else {
                    format!(
                        "done: kept whole, {:.1}s marked as commercial in the chapters, \
                         confidence {:.2}",
                        result.plan.cut_seconds().max(0.0),
                        result.plan.confidence
                    )
                },
            )
            .await;
        }
        // Waiting for a logo is not a failure and must not be coloured as
        // one: nothing went wrong, and the queue is telling you what it needs.
        Ok(Err(asaborake_core::Error::NeedsLogo)) => {
            let message = asaborake_core::Error::NeedsLogo.to_string();
            tracing::info!(job = %job.id, "blocked, waiting for a logo");
            let _ = context
                .store
                .finish(&job.id, JobStatus::Blocked, Some(&message), None, None)
                .await;
            log(context, &job.id, "warn", &message).await;
        }
        Ok(Err(error)) => fail(context, job, &explain(&error)).await,
        // The blocking task itself failed, which means a panic in the
        // pipeline. That is a defect, and the job must not look successful.
        Err(error) => fail(context, job, &format!("worker stopped: {error}")).await,
    }

    Ok(())
}

/// How this recording should be treated, from the channel and the rules.
///
/// The channel is the general case and a rule is the particular one, so a
/// matching rule has the last word.
async fn settings_for(context: &Context, job: &Job) -> asaborake_core::ChannelSettings {
    let mut settings = asaborake_core::ChannelStore::open(&context.config.channels)
        .get(job.channel_id.as_deref().unwrap_or_default());

    // A rule names a more particular case than a whole channel does, so where
    // one matches it has the last word.
    let matched = asaborake_core::RuleSet::open(&context.config.rules).first_match(
        &asaborake_core::Candidate {
            channel_id: job.channel_id.clone(),
            title: job.title.clone(),
            path: Some(job.input.clone()),
            // The picture size is not known until the source is probed, which
            // happens inside the pipeline. Rules that ask about it match only
            // once that is hoisted out; until then they simply do not fire.
            height: None,
        },
    );
    if let Some(rule) = &matched {
        if let Some(profile) = &rule.profile {
            settings.profile = Some(profile.clone());
        }
        if let Some(detect) = rule.detect_commercials {
            settings.detect_commercials = detect;
        }
        log(
            context,
            &job.id,
            "info",
            &format!(
                "matched the rule '{}'",
                rule.name.as_deref().unwrap_or("(unnamed)")
            ),
        )
        .await;
    }

    settings
}

/// Name the output after the programme, when a template says how.
///
/// `EPGStation` hands over a path it chose; everything needed to file the
/// result properly is already known. Any directories the template builds are
/// created here rather than being discovered missing by ffmpeg an hour later.
async fn rename_output(context: &Context, mut job: Job) -> Job {
    let Some(template) = context.config.output_template.as_deref() else {
        return job;
    };

    let fields = asaborake_core::Fields {
        title: job.title.clone(),
        channel: job.channel_name.clone().or_else(|| job.channel_id.clone()),
        recorded_at: Some(job.created_at.into()),
        source: std::path::Path::new(&job.input)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned()),
    };

    let Some(renamed) =
        asaborake_core::rename(std::path::Path::new(&job.output), template, &fields)
    else {
        return job;
    };
    if renamed == std::path::Path::new(&job.output) {
        return job;
    }

    if let Some(parent) = renamed.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(%error, path = %parent.display(), "cannot make the output directory");
        return job;
    }

    let renamed = renamed.to_string_lossy().into_owned();
    log(
        context,
        &job.id,
        "info",
        &format!("writing to {renamed} rather than {}", job.output),
    )
    .await;
    let _ = context.store.set_output(&job.id, &renamed).await;
    job.output = renamed;
    job
}

/// Why this job cannot start for lack of disk, if that is the case.
///
/// An encode that runs out of room half way through has cost an hour of GPU
/// time and leaves a truncated file that looks real until somebody plays it.
fn no_room_for(job: &Job) -> Option<String> {
    let short = crate::disk::shortfall(
        std::path::Path::new(&job.input),
        std::path::Path::new(&job.output),
    )?;
    Some(format!(
        "not enough room where the output goes — about {} short of what this \
         recording is likely to need",
        crate::disk::describe(short)
    ))
}

/// Render an error together with everything underneath it.
///
/// `thiserror` types print only their own message, so a pipeline failure that
/// began with an ffmpeg exit code reaches the operator as the word "analysis
/// error" — which says nothing at all. Walking the source chain is what turns
/// it back into something that can be acted on.
fn explain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        // Some layers restate their child rather than adding to it; repeating
        // that back makes the line harder to read, not easier.
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}

/// Scan the source and record what it is before the work begins.
///
/// Doing this up front rather than folding it into the pipeline's result is
/// what keeps it on a job that then fails — and a recording damaged enough to
/// fail analysis is precisely the one whose diagnostics explain why.
async fn inspect_source(context: &Context, job: &Job) -> Option<asaborake_core::Diagnostics> {
    let input = std::path::PathBuf::from(&job.input);
    // A full pass over a multi-gigabyte file; not something to do on the
    // async runtime's threads.
    let diagnostics = tokio::task::spawn_blocking(move || asaborake_core::inspect(&input))
        .await
        .ok()
        .flatten()?;

    if let Ok(text) = serde_json::to_string(&diagnostics) {
        let _ = context.store.set_diagnostics(&job.id, &text).await;
    }
    for warning in &diagnostics.warnings {
        log(context, &job.id, "warn", warning).await;
    }
    publish(context, &job.id).await;

    Some(diagnostics)
}

/// Record a failure against a job.
async fn fail(context: &Context, job: &Job, message: &str) {
    tracing::error!(job = %job.id, message, "job failed");
    let _ = context
        .store
        .finish(&job.id, JobStatus::Failed, Some(message), None, None)
        .await;
    log(context, &job.id, "error", message).await;
}

/// Append a log line and publish it.
async fn log(context: &Context, id: &str, level: &str, message: &str) {
    let _ = context.store.log(id, level, message).await;
    context.events.publish(Update::Log {
        job_id: id.to_owned(),
        message: message.to_owned(),
    });
}

/// Publish a job's current state.
async fn publish(context: &Context, id: &str) {
    if let Ok(Some(job)) = context.store.get(id).await {
        context.events.publish(Update::Job { job: Box::new(job) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_is_reported_with_what_caused_it() {
        // "analysis error" on its own tells an operator nothing; the ffmpeg
        // exit code underneath it is the part worth reading.
        #[derive(Debug, thiserror::Error)]
        #[error("ffmpeg exited with status 1")]
        struct Ffmpeg;

        #[derive(Debug, thiserror::Error)]
        #[error("analysis error")]
        struct Analysis(#[source] Ffmpeg);

        assert_eq!(
            explain(&Analysis(Ffmpeg)),
            "analysis error: ffmpeg exited with status 1"
        );
    }

    #[test]
    fn a_cause_that_only_restates_its_parent_is_not_repeated() {
        #[derive(Debug, thiserror::Error)]
        #[error("no such file")]
        struct Inner;

        #[derive(Debug, thiserror::Error)]
        #[error("no such file")]
        struct Outer(#[source] Inner);

        assert_eq!(explain(&Outer(Inner)), "no such file");
    }

    #[test]
    fn a_dropped_update_does_not_block_the_publisher() {
        let events = Events::new();
        // No subscribers: publishing must be a no-op rather than an error.
        events.publish(Update::Log {
            job_id: "x".into(),
            message: "hello".into(),
        });
    }

    #[tokio::test]
    async fn subscribers_receive_updates() {
        let events = Events::new();
        let mut receiver = events.subscribe();

        events.publish(Update::Log {
            job_id: "job-1".into(),
            message: "encoding".into(),
        });

        let update = receiver.recv().await.expect("an update");
        assert_eq!(
            update,
            Update::Log {
                job_id: "job-1".into(),
                message: "encoding".into()
            }
        );
    }

    #[test]
    fn updates_serialise_with_a_discriminator_the_client_can_switch_on() {
        let update = Update::Log {
            job_id: "a".into(),
            message: "b".into(),
        };
        let json = serde_json::to_string(&update).expect("serialises");
        assert!(json.contains(r#""type":"log""#), "{json}");
    }
}
