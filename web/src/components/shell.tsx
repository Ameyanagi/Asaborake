/**
 * The pieces every view is built from.
 *
 * Kept deliberately few: a page header, a readout, an empty state, a failure.
 * The design has no cards, so there is no card component; content sits
 * directly on the ground, separated by the graticule.
 */

import type { ReactNode } from "react";

export function Page({
  title,
  detail,
  aside,
  children,
}: {
  title: string;
  detail?: string | undefined;
  aside?: ReactNode | undefined;
  children: ReactNode;
}) {
  // A measure, not a full-bleed page. Dense rows read as single units only
  // while their two ends stay within a glance of each other; stretched across
  // a wide monitor they fall apart into two disconnected columns.
  return (
    <div className="min-h-full">
      <header className="border-b border-rule">
        <div className="mx-auto flex max-w-6xl items-end justify-between gap-6 px-6 py-5">
          <div>
            <h1 className="text-[17px] tracking-[0.06em] text-ink">{title}</h1>
            {detail && <p className="eyebrow mt-1.5">{detail}</p>}
          </div>
          {aside}
        </div>
      </header>
      <div className="mx-auto max-w-6xl">{children}</div>
    </div>
  );
}

/**
 * A labelled value.
 *
 * The label sits under the number, not beside it, so a row of readouts scans
 * as a row of numbers first.
 */
export function Readout({
  label,
  value,
  tone,
}: {
  label: string;
  value: ReactNode;
  tone?: "good" | "alert" | "signal" | undefined;
}) {
  const colour =
    tone === "good"
      ? "text-good"
      : tone === "alert"
        ? "text-alert"
        : tone === "signal"
          ? "text-programme"
          : "text-ink";

  return (
    <div>
      <div className={`text-[16px] tabular-nums ${colour}`}>{value}</div>
      <div className="eyebrow mt-1">{label}</div>
    </div>
  );
}

/** An empty screen is an invitation to act, not an apology. */
export function Empty({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="px-6 py-16">
      <p className="text-ink">{title}</p>
      <p className="mt-2 max-w-lg font-sans leading-relaxed text-ink-dim">
        {detail}
      </p>
    </div>
  );
}

/** What went wrong, and what to do about it. Never vague, never apologetic. */
export function Failure({ message }: { message: string }) {
  return (
    <div className="mx-6 mt-6 border-l-2 border-alert bg-panel px-4 py-3">
      <div className="eyebrow" style={{ color: "var(--color-alert)" }}>
        failed
      </div>
      {/*
        An engine failure carries its cause underneath it, one per line, and a
        tail of ffmpeg's own output. Collapsing that into a paragraph runs
        separate facts together into something nobody will read, so the breaks
        are kept and the mono face makes the ffmpeg lines legible.
      */}
      <p className="mt-1.5 max-h-64 overflow-y-auto text-[13px] leading-relaxed whitespace-pre-wrap text-ink">
        {message}
      </p>
    </div>
  );
}

/**
 * Something worth knowing that did not stop the job.
 *
 * Distinct from `Failure` on purpose: a recording with poor reception still
 * produced a file, and colouring that the same red as a failed job would teach
 * a reader to ignore the colour.
 */
export function Notice({ messages }: { messages: string[] }) {
  if (messages.length === 0) return null;
  return (
    <div className="mx-6 mt-6 border-l-2 border-programme bg-panel px-4 py-3">
      <div className="eyebrow" style={{ color: "var(--color-programme)" }}>
        {messages.length === 1 ? "note" : "notes"}
      </div>
      <ul className="mt-1.5 space-y-1 font-sans text-ink">
        {messages.map((message) => (
          <li key={message}>{message}</li>
        ))}
      </ul>
    </div>
  );
}

/** A quiet action. Buttons say exactly what happens when they are used. */
export function Action({
  onClick,
  children,
  tone,
  disabled,
}: {
  onClick: () => void;
  children: ReactNode;
  tone?: "alert" | undefined;
  disabled?: boolean | undefined;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`border px-3 py-1.5 transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
        tone === "alert"
          ? "border-rule-bright text-ink-dim hover:border-alert hover:text-alert"
          : "border-rule-bright text-ink-dim hover:border-programme hover:text-programme"
      }`}
    >
      {children}
    </button>
  );
}
