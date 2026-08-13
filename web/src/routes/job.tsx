/**
 * One job.
 *
 * The timeline is the page. Everything else — the readouts above it, the
 * segment list beside it, the log below — exists to explain what the timeline
 * shows. A reader arrives here to answer one question: did it cut the right
 * thing?
 */

import { useEffect, useState } from "react";
import { useParams } from "@tanstack/react-router";
import {
  api,
  formatDuration,
  subscribe,
  type Analysis,
  type CutPlan,
  type Job,
  type JobEvent,
  type Segment,
} from "../lib/api";
import { Timeline } from "../components/Timeline";
import { Action, Empty, Failure, Page, Readout } from "../components/shell";

export function JobDetail() {
  const { jobId } = useParams({ from: "/jobs/$jobId" });

  const [job, setJob] = useState<Job | null>(null);
  const [analysis, setAnalysis] = useState<Analysis | null>(null);
  const [plan, setPlan] = useState<CutPlan | null>(null);
  const [events, setEvents] = useState<JobEvent[]>([]);
  const [selected, setSelected] = useState<Segment | undefined>();
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;

    const load = () => {
      void api
        .getJob(jobId)
        .then((loaded) => live && setJob(loaded))
        .catch((cause: Error) => live && setError(cause.message));
      void api
        .jobAnalysis(jobId)
        .then((loaded) => {
          if (!live) return;
          setAnalysis(loaded.analysis);
          setPlan(loaded.plan);
        })
        .catch(() => {});
      void api
        .jobEvents(jobId)
        .then((loaded) => live && setEvents(loaded))
        .catch(() => {});
    };

    load();

    const stop = subscribe((update) => {
      if (update.type === "job" && update.job.id === jobId) {
        setJob(update.job);
        // The analysis only exists once the job has finished, so it is
        // fetched again when the status settles rather than polled for.
        if (update.job.status === "completed") load();
      }
      if (update.type === "log" && update.job_id === jobId) {
        void api.jobEvents(jobId).then((loaded) => live && setEvents(loaded));
      }
    });

    return () => {
      live = false;
      stop();
    };
  }, [jobId]);

  if (error) {
    return (
      <Page title="Job">
        <Failure message={error} />
      </Page>
    );
  }
  if (!job) {
    return (
      <Page title="Job">
        <Empty title="Loading" detail="Fetching this job from the engine." />
      </Page>
    );
  }

  const name = job.title ?? job.input.split("/").pop() ?? job.input;
  const cut =
    plan?.segments
      .filter((segment) => segment.kind === "commercial")
      .reduce((total, segment) => total + (segment.end - segment.start), 0) ?? 0;

  return (
    <Page
      title={name}
      detail={`${job.profile} · ${job.channel_name ?? job.channel_id ?? "unknown channel"}`}
      aside={
        <div className="flex gap-3">
          {(job.status === "queued" || job.status === "running") && (
            <Action tone="alert" onClick={() => void api.cancelJob(job.id)}>
              Stop
            </Action>
          )}
          {(job.status === "failed" || job.status === "cancelled") && (
            <Action onClick={() => void api.retryJob(job.id)}>Run again</Action>
          )}
        </div>
      }
    >
      {job.error && <Failure message={job.error} />}

      <section className="flex flex-wrap gap-10 border-b border-rule px-6 py-5">
        <Readout
          label="status"
          value={job.status}
          tone={
            job.status === "failed"
              ? "alert"
              : job.status === "completed"
                ? "good"
                : job.status === "running"
                  ? "signal"
                  : undefined
          }
        />
        {analysis && (
          <Readout
            label="recording"
            value={formatDuration(analysis.duration_seconds)}
          />
        )}
        {plan && (
          <>
            <Readout label="removed" value={formatDuration(cut)} tone="signal" />
            <Readout label="confidence" value={plan.confidence.toFixed(2)} />
            <Readout
              label="decision"
              value={plan.decision === "cut" ? "cut" : "kept whole"}
            />
          </>
        )}
        {analysis?.logo && (
          <Readout
            label="logo opacity"
            value={analysis.logo.mean_alpha.toFixed(2)}
          />
        )}
      </section>

      {plan && (
        <p className="border-b border-rule px-6 py-3 font-sans text-ink-dim">
          {plan.reason}
        </p>
      )}

      {analysis && plan ? (
        <section className="px-6 py-6">
          <h2 className="eyebrow mb-4">Timeline</h2>
          <Timeline
            analysis={analysis}
            plan={plan}
            selected={selected}
            onSelect={setSelected}
          />

          <div className="mt-8 border-t border-rule">
            {plan.segments.map((segment) => {
              const isSelected =
                selected?.start === segment.start &&
                selected.kind === segment.kind;
              return (
                <button
                  key={`${segment.kind}-${segment.start}`}
                  type="button"
                  onClick={() => setSelected(segment)}
                  className={`rule-row flex w-full items-baseline gap-6 px-2 py-2 text-left tabular-nums ${
                    isSelected ? "bg-raised" : ""
                  }`}
                >
                  <span
                    className="inline-block h-2 w-4 shrink-0"
                    style={{
                      background:
                        segment.kind === "programme"
                          ? "var(--color-programme)"
                          : "var(--color-commercial)",
                    }}
                    aria-hidden="true"
                  />
                  <span className="w-24 shrink-0 text-ink">
                    {segment.kind === "programme" ? "Programme" : "CM"}
                  </span>
                  <span className="w-40 shrink-0 text-ink-dim">
                    {formatDuration(segment.start)} –{" "}
                    {formatDuration(segment.end)}
                  </span>
                  <span className="w-20 shrink-0 text-ink-dim">
                    {formatDuration(segment.end - segment.start)}
                  </span>
                  <span className="text-ink-faint">
                    confidence {segment.confidence.toFixed(2)}
                  </span>
                </button>
              );
            })}
          </div>
        </section>
      ) : (
        job.status === "completed" && (
          <Empty
            title="No analysis was stored for this job"
            detail="It was transcoded by a version that did not keep one, or the record was removed."
          />
        )
      )}

      <section className="border-t border-rule px-6 py-5">
        <h2 className="eyebrow mb-3">Log</h2>
        {events.length === 0 ? (
          <p className="text-ink-faint">Nothing logged yet.</p>
        ) : (
          <ol className="space-y-1">
            {events.map((event) => (
              <li key={event.id} className="flex gap-4">
                <span className="w-20 shrink-0 tabular-nums text-ink-faint">
                  {new Date(event.at).toLocaleTimeString()}
                </span>
                <span
                  className={
                    event.level === "error"
                      ? "text-alert"
                      : event.level === "warn"
                        ? "text-programme"
                        : "text-ink-dim"
                  }
                >
                  {event.message}
                </span>
              </li>
            ))}
          </ol>
        )}
      </section>
    </Page>
  );
}
