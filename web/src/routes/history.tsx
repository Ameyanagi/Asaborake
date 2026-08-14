/**
 * What the machine has actually done.
 *
 * The queue answers "is anything wrong right now". This answers the slower
 * question: is it working *well* — is it keeping up, is one channel always
 * failing, did last night's batch finish before morning. Those need the
 * numbers a single job never shows, so this is a table of them.
 */

import { useEffect, useMemo, useState } from "react";
import { Link } from "@tanstack/react-router";
import { api, formatDuration, type Job } from "../lib/api";
import { Empty, Failure, Page } from "../components/shell";

/** What the table can be ordered by. */
type Sort = "finished" | "elapsed" | "size" | "title";

export function History() {
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<Sort>("finished");

  useEffect(() => {
    void api
      .listJobs(500)
      .then(setJobs)
      .catch((cause: Error) => setError(cause.message));
  }, []);

  /** How long a job took, in seconds, once it had started. */
  const elapsed = (job: Job): number | null => {
    if (!job.started_at || !job.finished_at) return null;
    const seconds =
      (new Date(job.finished_at).getTime() - new Date(job.started_at).getTime()) / 1000;
    return seconds > 0 ? seconds : null;
  };

  const finished = useMemo(() => {
    const done = (jobs ?? []).filter((job) => job.finished_at);
    const ordered = [...done];
    ordered.sort((a, b) => {
      switch (sort) {
        case "elapsed":
          return (elapsed(b) ?? 0) - (elapsed(a) ?? 0);
        case "size":
          return (b.output_bytes ?? 0) - (a.output_bytes ?? 0);
        case "title":
          return (a.title ?? a.input).localeCompare(b.title ?? b.input);
        default:
          return (
            new Date(b.finished_at ?? 0).getTime() -
            new Date(a.finished_at ?? 0).getTime()
          );
      }
    });
    return ordered;
  }, [jobs, sort]);

  const completed = finished.filter((job) => job.status === "completed");
  const failed = finished.filter((job) => job.status === "failed");
  // Total time spent is the number that says whether the machine is keeping
  // up, which no single row can.
  const spent = completed.reduce((total, job) => total + (elapsed(job) ?? 0), 0);

  return (
    <Page
      title="History"
      detail={
        finished.length > 0
          ? `${completed.length} done · ${failed.length} failed · ${formatDuration(spent)} of encoding`
          : undefined
      }
    >
      {error && <Failure message={error} />}

      {jobs && finished.length === 0 && (
        <Empty
          title="Nothing has finished yet"
          detail="Jobs appear here once they have run, whether they succeeded or not."
        />
      )}

      {finished.length > 0 && (
        <>
          <div className="flex flex-wrap items-center gap-4 border-b border-rule px-6 py-3">
            <span className="eyebrow">order by</span>
            {(
              [
                ["finished", "most recent"],
                ["elapsed", "longest"],
                ["size", "largest"],
                ["title", "title"],
              ] as [Sort, string][]
            ).map(([value, label]) => (
              <button
                key={value}
                type="button"
                onClick={() => setSort(value)}
                className={`border px-2.5 py-1 transition-colors ${
                  sort === value
                    ? "border-programme text-programme"
                    : "border-rule-bright text-ink-dim hover:text-ink"
                }`}
              >
                {label}
              </button>
            ))}
          </div>

          <div className="border-t border-rule">
            {finished.map((job) => (
              <Link
                key={job.id}
                to="/jobs/$jobId"
                params={{ jobId: job.id }}
                className="rule-row flex items-baseline gap-6 px-6 py-3 tabular-nums"
              >
                <span
                  className={`w-16 shrink-0 ${
                    job.status === "completed"
                      ? "text-good"
                      : job.status === "failed"
                        ? "text-alert"
                        : "text-ink-faint"
                  }`}
                >
                  {job.status === "completed" ? "done" : job.status}
                </span>
                <span className="min-w-0 flex-1 truncate text-ink">
                  {job.title ?? job.input.split("/").pop() ?? job.input}
                </span>
                <span className="w-28 shrink-0 truncate text-ink-dim">
                  {job.channel_name ?? job.channel_id ?? ""}
                </span>
                <span className="w-24 shrink-0 text-ink-dim">{job.profile}</span>
                <span className="w-20 shrink-0 text-ink-dim">
                  {(() => {
                    const took = elapsed(job);
                    return took === null ? "—" : formatDuration(took);
                  })()}
                </span>
                <span className="w-20 shrink-0 text-right text-ink-dim">
                  {job.output_bytes ? describeSize(job.output_bytes) : "—"}
                </span>
                <span className="w-28 shrink-0 text-right text-ink-faint">
                  {job.finished_at
                    ? new Date(job.finished_at).toLocaleDateString([], {
                        month: "short",
                        day: "numeric",
                      }) +
                      " " +
                      new Date(job.finished_at).toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                        hour12: false,
                      })
                    : ""}
                </span>
              </Link>
            ))}
          </div>
        </>
      )}
    </Page>
  );
}

/** A size a person can read. */
function describeSize(bytes: number): string {
  const gib = bytes / (1024 * 1024 * 1024);
  return gib >= 1 ? `${gib.toFixed(1)} GiB` : `${Math.round(bytes / (1024 * 1024))} MiB`;
}
