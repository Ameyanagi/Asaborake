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
import { SubmitJob } from "../components/SubmitJob";

export function Dashboard() {
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [only, setOnly] = useState<JobStatus | "all">("all");

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

  // Filtering happens over what is already loaded rather than by asking the
  // engine again: the whole queue is here, it arrives as a live stream, and a
  // round trip per keystroke would make the filter lag the typing.
  const shown = (jobs ?? []).filter((job) => {
    if (only !== "all" && job.status !== only) return false;
    if (!search.trim()) return true;
    const needle = search.trim().toLowerCase();
    return [job.title, job.input, job.channel_name, job.profile]
      .filter((field): field is string => Boolean(field))
      .some((field) => field.toLowerCase().includes(needle));
  });

  return (
    <Page
      title="Queue"
      detail={
        health
          ? `engine ${health.version} · ffmpeg ${health.ffmpeg}`
          : "connecting to the engine"
      }
      aside={
        <div className="flex items-center gap-6 tabular-nums">
          <Count value={running} label="running" lit={running > 0} />
          <Count value={queued} label="queued" />
          <SubmitJob
            onSubmitted={() => {
              void api.listJobs().then(setJobs).catch(() => {});
            }}
          />
        </div>
      }
    >
      {error && <Failure message={error} />}

      {jobs?.length === 0 && (
        <Empty
          title="Nothing has been transcoded yet"
          detail="Jobs appear here when EPGStation finishes a recording, or when you submit one from here."
        />
      )}

      {jobs && jobs.length > 0 && (
        <>
          <div className="flex flex-wrap items-center gap-4 border-b border-rule px-6 py-3">
            <input
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="search title, file, channel or profile"
              className="w-72 border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
            />
            <div className="flex flex-wrap gap-1.5">
              {STATUS_FILTERS.map(({ value, label }) => (
                <button
                  key={value}
                  type="button"
                  onClick={() => setOnly(value)}
                  className={`border px-2.5 py-1 transition-colors ${
                    only === value
                      ? "border-programme text-programme"
                      : "border-rule-bright text-ink-dim hover:text-ink"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
            {shown.length !== jobs.length && (
              <span className="tabular-nums text-ink-faint">
                {shown.length} of {jobs.length}
              </span>
            )}
          </div>

          {shown.length === 0 ? (
            <Empty
              title="Nothing matches"
              detail="No job in the queue matches that search and filter."
            />
          ) : (
            <div className="border-t border-rule">
              {shown.map((job) => (
                <JobRow key={job.id} job={job} />
              ))}
            </div>
          )}
        </>
      )}
    </Page>
  );
}

/**
 * The filters worth offering.
 *
 * Not one per status: "stopped" and "needs logo" are rare enough that a chip
 * each would be mostly dead furniture, while "failed" is the one somebody
 * comes to this screen looking for.
 */
const STATUS_FILTERS: { value: JobStatus | "all"; label: string }[] = [
  { value: "all", label: "all" },
  { value: "running", label: "running" },
  { value: "queued", label: "queued" },
  { value: "completed", label: "done" },
  { value: "failed", label: "failed" },
  { value: "blocked", label: "needs logo" },
];

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
          <div className="h-[3px] w-64 shrink-0 bg-rule">
            <div
              className="h-full bg-programme transition-[width] duration-500"
              style={{ width: `${Math.round(job.progress * 100)}%` }}
            />
          </div>
          <span className="w-10 shrink-0 text-right tabular-nums text-ink-dim">
            {Math.round(job.progress * 100)}%
          </span>
          <span className="min-w-0 flex-1 truncate text-ink-faint">
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
    // Amber rather than red: it is waiting, not broken.
    blocked: { colour: "var(--color-programme)", label: "needs logo" },
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
