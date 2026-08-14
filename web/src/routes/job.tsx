/**
 * One job.
 *
 * The timeline is the page. Everything else — the readouts above it, the
 * segment list beside it, the log below — exists to explain what the timeline
 * shows. A reader arrives here to answer one question: did it cut the right
 * thing?
 */

import { Fragment, useEffect, useState } from "react";
import { useParams } from "@tanstack/react-router";
import {
  api,
  formatDuration,
  subscribe,
  type Analysis,
  type CutPlan,
  type Diagnostics,
  type Job,
  type JobEvent,
  type Segment,
  type SegmentKind,
} from "../lib/api";
import { Timeline } from "../components/Timeline";
import {
  Action,
  Empty,
  Failure,
  Notice,
  Page,
  Readout,
} from "../components/shell";

/**
 * What the recording was and how cleanly it arrived.
 *
 * The counters are shown as a share of the whole rather than raw totals: four
 * hundred lost packets is either nothing or a broken aerial depending entirely
 * on how many there were.
 */
function Source({ source }: { source: Diagnostics }) {
  const share = (count: number) => {
    if (count === 0) return "none";
    const percent = (count / Math.max(source.total_packets, 1)) * 100;
    return `${count.toLocaleString()} · ${
      percent < 0.01 ? "<0.01" : percent.toFixed(2)
    }%`;
  };

  return (
    <section className="border-b border-rule px-6 py-5">
      <h2 className="eyebrow mb-4">Source</h2>

      <dl className="grid gap-x-10 gap-y-2 sm:grid-cols-[9rem_1fr]">
        <dt className="text-ink-faint">Picture</dt>
        <dd className="text-ink-dim tabular-nums">
          {source.video ?? "no video stream"}
          {source.format_changes.length > 0 &&
            `, changing at ${source.format_changes
              .map((at) => formatDuration(at))
              .join(", ")}`}
        </dd>

        <dt className="text-ink-faint">Audio</dt>
        <dd className="text-ink-dim tabular-nums">
          {source.audio.length === 0
            ? "no audio stream"
            : source.audio.join(" · ")}
          {source.dual_mono &&
            `, bilingual (${source.dual_mono.main ?? "unknown"} and ${
              source.dual_mono.sub ?? "unknown"
            })`}
        </dd>

        <dt className="text-ink-faint">Captions</dt>
        <dd className="text-ink-dim">
          {source.has_captions ? "present" : "none"}
        </dd>

        <dt className="text-ink-faint">Lost</dt>
        <dd className="text-ink-dim tabular-nums">
          {share(source.dropped_packets)}
        </dd>

        <dt className="text-ink-faint">Scrambled</dt>
        <dd
          className={`tabular-nums ${
            source.scrambled_packets > 0 ? "text-alert" : "text-ink-dim"
          }`}
        >
          {share(source.scrambled_packets)}
        </dd>

        <dt className="text-ink-faint">Corrupt</dt>
        <dd className="text-ink-dim tabular-nums">
          {share(source.error_packets)}
        </dd>

        <dt className="text-ink-faint">Packets read</dt>
        <dd className="text-ink-dim tabular-nums">
          {source.total_packets.toLocaleString()}
        </dd>
      </dl>
    </section>
  );
}

/**
 * Why this boundary, and not one a second either side.
 *
 * The timeline shows the decision and the evidence side by side, but not the
 * *link* between them — and when a cut lands somewhere surprising, that link
 * is the only thing worth looking at. Amatsukaze cannot answer this at all:
 * its detection is a rule cascade with no record of which rule fired.
 *
 * Everything here is computed from the analysis already loaded, so it costs
 * nothing and cannot disagree with what the timeline drew.
 */
function Evidence({
  segment,
  analysis,
}: {
  segment: Segment;
  analysis: Analysis;
}) {
  /** The strongest cut within half a second of a moment. */
  const cutNear = (at: number) => {
    const near = analysis.scene_changes.filter(
      (change) => Math.abs(change.seconds - at) <= 0.5,
    );
    return near.sort((a, b) => b.strength - a.strength)[0];
  };

  /** A silence covering a moment, if there is one. */
  const silenceAt = (at: number) =>
    analysis.silent_spans.find(
      (span) => span.start - 0.5 <= at && span.end + 0.5 >= at,
    );

  /** How much of this stretch the logo was up for. */
  const logoShare = (() => {
    const covered = analysis.logo_intervals.reduce((total, interval) => {
      const start = Math.max(interval.start, segment.start);
      const end = Math.min(interval.end, segment.end);
      return total + Math.max(0, end - start);
    }, 0);
    const length = segment.end - segment.start;
    return length > 0 ? covered / length : 0;
  })();

  const length = segment.end - segment.start;
  // Japanese commercial blocks are laid out on a fifteen-second grid, and how
  // near a block sits to it is one of the strongest signals there is.
  const offGrid = Math.abs(length - Math.round(length / 15) * 15);

  const rows: [string, string, boolean][] = [
    [
      "logo",
      segment.kind === "commercial"
        ? `absent for ${((1 - logoShare) * 100).toFixed(0)}% of this stretch`
        : `present for ${(logoShare * 100).toFixed(0)}% of this stretch`,
      segment.kind === "commercial" ? logoShare < 0.2 : logoShare > 0.8,
    ],
    [
      "length",
      `${formatDuration(length)}, ${
        offGrid < 0.35
          ? `on the 15-second grid (${Math.round(length / 15)} × 15s)`
          : `${offGrid.toFixed(1)}s off the 15-second grid`
      }`,
      offGrid < 0.35,
    ],
    [
      "starts at",
      [
        cutNear(segment.start)
          ? `a scene change (strength ${cutNear(segment.start)?.strength.toFixed(2)})`
          : "no scene change nearby",
        silenceAt(segment.start) ? "silence" : "no silence",
      ].join(", "),
      Boolean(cutNear(segment.start)) && Boolean(silenceAt(segment.start)),
    ],
    [
      "ends at",
      [
        cutNear(segment.end)
          ? `a scene change (strength ${cutNear(segment.end)?.strength.toFixed(2)})`
          : "no scene change nearby",
        silenceAt(segment.end) ? "silence" : "no silence",
      ].join(", "),
      Boolean(cutNear(segment.end)) && Boolean(silenceAt(segment.end)),
    ],
  ];

  return (
    <div className="mt-6 border-l-2 border-rule-bright bg-panel px-4 py-3">
      <div className="eyebrow">
        why this is {segment.kind === "commercial" ? "a commercial" : "programme"}
      </div>
      <dl className="mt-2 grid gap-x-8 gap-y-1.5 sm:grid-cols-[6rem_1fr]">
        {rows.map(([label, detail, agrees]) => (
          <Fragment key={label}>
            <dt className="text-ink-faint">{label}</dt>
            {/* Coloured by whether the evidence supports the label, because
                the useful reading is "which of these disagrees". */}
            <dd className={agrees ? "text-ink" : "text-ink-dim"}>{detail}</dd>
          </Fragment>
        ))}
      </dl>
      <p className="mt-2 font-sans text-ink-faint">
        Confidence {segment.confidence.toFixed(2)}. Highlighted evidence is
        what supports the label; the rest is what argued against it.
      </p>
    </div>
  );
}

export function JobDetail() {
  const { jobId } = useParams({ from: "/jobs/$jobId" });

  const [job, setJob] = useState<Job | null>(null);
  const [analysis, setAnalysis] = useState<Analysis | null>(null);
  const [plan, setPlan] = useState<CutPlan | null>(null);
  const [source, setSource] = useState<Diagnostics | null>(null);
  const [events, setEvents] = useState<JobEvent[]>([]);
  const [selected, setSelected] = useState<Segment | undefined>();
  // Segments somebody has retyped. Held here rather than written back, so the
  // original decision is still on screen to compare against until they commit.
  const [flipped, setFlipped] = useState<Record<string, SegmentKind>>({});
  const [recutting, setRecutting] = useState(false);
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
          setSource(loaded.diagnostics);
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
  // What was labelled commercial, which is only what was removed when the
  // plan was confident enough to cut. Below that bar the labels become
  // chapters and the recording is kept whole.
  const commercial =
    plan?.segments
      .filter((segment) => segment.kind === "commercial")
      .reduce((total, segment) => total + (segment.end - segment.start), 0) ?? 0;
  const wasCut = plan?.decision === "cut";

  /** A segment's identity, which is where it starts. */
  const key = (segment: Segment) => `${segment.start}`;

  /** Call a segment the other thing, or put it back. */
  const flip = (segment: Segment) => {
    setFlipped((current) => {
      const next = { ...current };
      const now = next[key(segment)] ?? segment.kind;
      const other: SegmentKind = now === "programme" ? "commercial" : "programme";
      if (other === segment.kind) {
        delete next[key(segment)];
      } else {
        next[key(segment)] = other;
      }
      return next;
    });
  };

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
          {(job.status === "failed" ||
            job.status === "cancelled" ||
            job.status === "blocked") && (
            <Action onClick={() => void api.retryJob(job.id)}>Run again</Action>
          )}
        </div>
      }
    >
      {/* A blocked job has not failed: it is waiting for something, and
          colouring it the same red as a failure would teach a reader that
          both mean the same thing. */}
      {job.error &&
        (job.status === "blocked" ? (
          <Notice messages={[job.error]} />
        ) : (
          <Failure message={job.error} />
        ))}
      {source && <Notice messages={source.warnings} />}

      <section className="flex flex-wrap gap-10 border-b border-rule px-6 py-5">
        <Readout
          label="status"
          value={job.status}
          tone={
            job.status === "failed"
              ? "alert"
              : job.status === "completed"
                ? "good"
                : job.status === "running" || job.status === "blocked"
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
            <Readout
              label={wasCut ? "removed" : "marked as CM"}
              value={formatDuration(commercial)}
              tone={wasCut ? "signal" : undefined}
            />
            <Readout label="confidence" value={plan.confidence.toFixed(2)} />
            <Readout label="decision" value={wasCut ? "cut" : "kept whole"} />
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

      {source && <Source source={source} />}

      {analysis && plan ? (
        <section className="px-6 py-6">
          <h2 className="eyebrow mb-4">Timeline</h2>
          <Timeline
            analysis={analysis}
            plan={plan}
            selected={selected}
            onSelect={setSelected}
          />

          {selected && <Evidence segment={selected} analysis={analysis} />}

          {Object.keys(flipped).length > 0 && (
            <div className="mt-6 flex flex-wrap items-center gap-4 border-l-2 border-programme bg-panel px-4 py-3">
              <span className="font-sans text-ink">
                {Object.keys(flipped).length} segment
                {Object.keys(flipped).length === 1 ? "" : "s"} retyped.
              </span>
              <Action
                disabled={recutting}
                onClick={() => {
                  setRecutting(true);
                  // Adjacent kept stretches are merged, because two ranges
                  // meeting at a point would cut and rejoin at that frame for
                  // no reason.
                  const keep: { start: number; end: number }[] = [];
                  for (const segment of plan.segments) {
                    const kind = flipped[key(segment)] ?? segment.kind;
                    if (kind !== "programme") continue;
                    const last = keep[keep.length - 1];
                    if (last && Math.abs(last.end - segment.start) < 0.001) {
                      last.end = segment.end;
                    } else {
                      keep.push({ start: segment.start, end: segment.end });
                    }
                  }
                  void api
                    .recutJob(job.id, keep)
                    .then(() => setFlipped({}))
                    .catch((cause: Error) => setError(cause.message))
                    .finally(() => setRecutting(false));
                }}
              >
                {recutting ? "Queueing…" : "Re-encode with these cuts"}
              </Action>
              <Action onClick={() => setFlipped({})}>Undo</Action>
              <span className="font-sans text-ink-dim">
                Written beside the original, not over it.
              </span>
            </div>
          )}

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
                        (flipped[key(segment)] ?? segment.kind) === "programme"
                          ? "var(--color-programme)"
                          : "var(--color-commercial)",
                    }}
                    aria-hidden="true"
                  />
                  <span className="w-24 shrink-0 text-ink">
                    {(flipped[key(segment)] ?? segment.kind) === "programme"
                      ? "Programme"
                      : "CM"}
                    {flipped[key(segment)] && (
                      <span className="ml-1 text-programme">·</span>
                    )}
                  </span>
                  <span className="w-40 shrink-0 text-ink-dim">
                    {formatDuration(segment.start)} –{" "}
                    {formatDuration(segment.end)}
                  </span>
                  <span className="w-20 shrink-0 text-ink-dim">
                    {formatDuration(segment.end - segment.start)}
                  </span>
                  <span className="flex-1 text-ink-faint">
                    confidence {segment.confidence.toFixed(2)}
                  </span>
                  {/* Retyping a segment is the correction that turns a wrong
                      detection from a lost recording into a moment's work. */}
                  <span
                    role="button"
                    tabIndex={0}
                    title="Call this the other thing"
                    onClick={(event) => {
                      event.stopPropagation();
                      flip(segment);
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.stopPropagation();
                        flip(segment);
                      }
                    }}
                    className="shrink-0 border border-rule-bright px-2 py-0.5 text-ink-dim transition-colors hover:border-programme hover:text-programme"
                  >
                    call it{" "}
                    {(flipped[key(segment)] ?? segment.kind) === "programme"
                      ? "CM"
                      : "programme"}
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
                <span className="w-20 shrink-0 whitespace-nowrap tabular-nums text-ink-faint">
                  {new Date(event.at).toLocaleTimeString([], {
                    hour12: false,
                  })}
                </span>
                <span
                  className={`whitespace-pre-wrap ${
                    event.level === "error"
                      ? "text-alert"
                      : event.level === "warn"
                        ? "text-programme"
                        : "text-ink-dim"
                  }`}
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
