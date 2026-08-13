/**
 * The queue.
 *
 * The question this view answers is "what happened overnight, and is anything
 * wrong". So the rows are dense and uniform, the progress of a running job is
 * part of the row rather than a separate widget, and a failure is the only
 * thing that gets a colour.
 */

import { useEffect, useState } from "react";
import { Link } from "@tanstack/react-router";
import {
  api,
  formatWhen,
  subscribe,
  type Health,
  type Job,
  type JobStatus,
} from "../lib/api";
import { Page, Empty, Failure } from "../components/shell";

export function Dashboard() {
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;

    void api
      .listJobs()
      .then((loaded) => live && setJobs(loaded))
      .catch((cause: Error) => live && setError(cause.message));
    void api.health().then((loaded) => live && setHealth(loaded)).catch(() => {});

    // Progress arrives as a stream of whole jobs, so a row is replaced rather
    // than patched. That keeps this view correct even if a message is missed.
    const stop = subscribe((update) => {
      if (update.type !== "job") return;
      setJobs((current) => {
        const rest = (current ?? []).filter((job) => job.id !== update.job.id);
        return [update.job, ...rest];
      });
    });

    return () => {
      live = false;
      stop();
    };
  }, []);

  const running = jobs?.filter((job) => job.status === "running").length ?? 0;
  const queued = jobs?.filter((job) => job.status === "queued").length ?? 0;

  return (
    <Page
      title="Queue"
      detail={
        health
          ? `engine ${health.version} · ffmpeg ${health.ffmpeg}`
          : "connecting to the engine"
      }
      aside={
        <div className="flex gap-6 tabular-nums">
          <Count value={running} label="running" lit={running > 0} />
          <Count value={queued} label="queued" />
        </div>
      }
    >
      {error && <Failure message={error} />}

      {jobs?.length === 0 && (
        <Empty
          title="Nothing has been transcoded yet"
          detail="Jobs appear here when EPGStation finishes a recording, or when you submit one over the API."
        />
      )}

      {jobs && jobs.length > 0 && (
        <div className="border-t border-rule">
          {jobs.map((job) => (
            <JobRow key={job.id} job={job} />
          ))}
        </div>
      )}
    </Page>
  );
}

function Count({
  value,
  label,
  lit = false,
}: {
  value: number;
  label: string;
  lit?: boolean | undefined;
}) {
  return (
    <div className="text-right">
      <div className={lit ? "text-[18px] text-programme" : "text-[18px]"}>
        {value}
      </div>
      <div className="eyebrow">{label}</div>
    </div>
  );
}

function JobRow({ job }: { job: Job }) {
  const name = job.title ?? basename(job.input);

  return (
    <Link
      to="/jobs/$jobId"
      params={{ jobId: job.id }}
      className="rule-row block px-6 py-3 transition-colors"
    >
      <div className="flex items-baseline gap-4">
        <StatusLamp status={job.status} />
        <span className="min-w-0 flex-1 truncate text-ink">{name}</span>
        <span className="hidden shrink-0 text-ink-faint sm:inline">
          {job.channel_name ?? job.channel_id ?? ""}
        </span>
        <span className="shrink-0 tabular-nums text-ink-dim">
          {formatWhen(job.created_at)}
        </span>
      </div>

      {job.status === "running" && (
        <div className="mt-2 flex items-center gap-3">
          {/* The bar is the row, not a component floating in it. */}
          <div className="h-[3px] flex-1 bg-rule">
            <div
              className="h-full bg-programme transition-[width] duration-500"
              style={{ width: `${Math.round(job.progress * 100)}%` }}
            />
          </div>
          <span className="w-10 shrink-0 text-right tabular-nums text-ink-dim">
            {Math.round(job.progress * 100)}%
          </span>
          <span className="w-56 shrink-0 truncate text-ink-faint">
            {job.message}
          </span>
        </div>
      )}

      {job.error && (
        <div className="mt-1.5 truncate text-alert">{job.error}</div>
      )}
    </Link>
  );
}

/**
 * A lit indicator, as on a rack unit: colour only where it means something.
 */
function StatusLamp({ status }: { status: JobStatus }) {
  const look: Record<JobStatus, { colour: string; label: string }> = {
    queued: { colour: "var(--color-ink-faint)", label: "queued" },
    running: { colour: "var(--color-programme)", label: "running" },
    completed: { colour: "var(--color-good)", label: "done" },
    failed: { colour: "var(--color-alert)", label: "failed" },
    cancelled: { colour: "var(--color-ink-faint)", label: "stopped" },
  };
  const { colour, label } = look[status];

  return (
    <span className="flex w-20 shrink-0 items-center gap-2">
      <span
        className="inline-block h-1.5 w-1.5 rounded-full"
        style={{ background: colour }}
        aria-hidden="true"
      />
      <span className="eyebrow" style={{ color: colour }}>
        {label}
      </span>
    </span>
  );
}

/** The filename, for when a recording has no title. */
function basename(path: string): string {
  return path.split("/").pop() ?? path;
}
