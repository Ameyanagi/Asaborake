/**
 * The timeline: what Asaborake measured, and what it decided.
 *
 * This is the whole product in one picture. Four tracks stacked against a
 * shared time axis, so the thing a reader needs to judge — whether the cuts
 * landed where the evidence says they should — is a vertical glance rather
 * than a comparison between separate views.
 *
 * The ruler is graduated in 15-second units because that is the grid Japanese
 * commercial blocks are laid out on, and the grid the segmenter scores
 * against. Drawing it makes the machine's central assumption visible: a block
 * that lines up with the graticule is one it was confident about.
 */

import { useCallback, useMemo, useRef, useState } from "react";
import type { Analysis, CutPlan, Segment } from "../lib/api";
import { formatDuration } from "../lib/api";

/** Heights of each track, in the SVG's own units. */
const TRACK = {
  ruler: 22,
  decision: 34,
  logo: 18,
  scene: 14,
  silence: 14,
  gap: 6,
} as const;

const HEIGHT =
  TRACK.ruler +
  TRACK.decision +
  TRACK.logo +
  TRACK.scene +
  TRACK.silence +
  TRACK.gap * 4;

/** The unit Japanese commercial blocks are built from. */
const GRID_SECONDS = 15;

interface TimelineProps {
  analysis: Analysis;
  plan: CutPlan;
  /** Called when a segment is clicked, for the detail panel. */
  onSelect?: ((segment: Segment) => void) | undefined;
  /** Which segment is currently selected. */
  selected?: Segment | undefined;
}

export function Timeline({
  analysis,
  plan,
  onSelect,
  selected,
}: TimelineProps) {
  const svg = useRef<SVGSVGElement>(null);
  const [hover, setHover] = useState<number | null>(null);

  const duration = analysis.duration_seconds || 1;

  /** Position along the axis, 0 to 1000 in the SVG's own units. */
  const x = useCallback((seconds: number) => (seconds / duration) * 1000, [
    duration,
  ]);

  /**
   * Ruler marks. Every 15 seconds is drawn, but only marks far enough apart to
   * stay legible get a label — on a three-hour recording that is every tenth.
   */
  const marks = useMemo(() => {
    const step = GRID_SECONDS;
    const count = Math.floor(duration / step);
    // Roughly one label per 90px of a 1000-unit axis.
    const labelEvery = Math.max(1, Math.ceil(count / 11));
    return Array.from({ length: count + 1 }, (_, index) => ({
      seconds: index * step,
      labelled: index % labelEvery === 0,
    }));
  }, [duration]);

  const trackTop = useMemo(() => {
    const decision = TRACK.ruler + TRACK.gap;
    const logo = decision + TRACK.decision + TRACK.gap;
    const scene = logo + TRACK.logo + TRACK.gap;
    const silence = scene + TRACK.scene + TRACK.gap;
    return { decision, logo, scene, silence };
  }, []);

  const onMove = (event: React.MouseEvent<SVGSVGElement>) => {
    const box = svg.current?.getBoundingClientRect();
    if (!box || box.width === 0) return;
    setHover(((event.clientX - box.left) / box.width) * duration);
  };

  return (
    <figure className="w-full">
      <svg
        ref={svg}
        viewBox={`0 0 1000 ${HEIGHT}`}
        preserveAspectRatio="none"
        className="w-full"
        style={{ height: `${HEIGHT * 3}px` }}
        onMouseMove={onMove}
        onMouseLeave={() => setHover(null)}
        role="img"
        aria-label={`Timeline of a ${formatDuration(duration)} recording with ${
          plan.segments.filter((s) => s.kind === "commercial").length
        } commercial blocks`}
      >
        {/* Graticule: every 15 seconds, the grid the detector rests on. */}
        <g>
          {marks.map((mark) => (
            <line
              key={mark.seconds}
              x1={x(mark.seconds)}
              x2={x(mark.seconds)}
              y1={mark.labelled ? 6 : 12}
              y2={HEIGHT}
              stroke="var(--color-rule)"
              strokeWidth={mark.labelled ? 1 : 0.5}
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {marks
            .filter((mark) => mark.labelled)
            .map((mark) => (
              <text
                key={`label-${mark.seconds}`}
                x={x(mark.seconds) + 3}
                y={12}
                fill="var(--color-ink-faint)"
                fontSize={9}
                fontFamily="var(--font-mono)"
              >
                {formatDuration(mark.seconds)}
              </text>
            ))}
        </g>

        {/* Decision: what is kept and what is removed. The tall track,
            because it is the answer and everything else is evidence. */}
        <g>
          {plan.segments.map((segment) => {
            const isProgramme = segment.kind === "programme";
            const isSelected =
              selected?.start === segment.start && selected.kind === segment.kind;
            return (
              <rect
                key={`${segment.kind}-${segment.start}`}
                x={x(segment.start)}
                y={trackTop.decision}
                width={Math.max(x(segment.end) - x(segment.start), 0.5)}
                height={TRACK.decision}
                fill={
                  isProgramme ? "var(--color-programme)" : "var(--color-commercial)"
                }
                fillOpacity={isProgramme ? 0.85 : 0.6}
                stroke={isSelected ? "var(--color-scene)" : "none"}
                strokeWidth={isSelected ? 1.5 : 0}
                vectorEffect="non-scaling-stroke"
                className="cursor-pointer"
                onClick={() => onSelect?.(segment)}
              >
                <title>
                  {segment.kind === "programme" ? "Programme" : "CM"}{" "}
                  {formatDuration(segment.start)}–{formatDuration(segment.end)} (
                  {formatDuration(segment.end - segment.start)}, confidence{" "}
                  {segment.confidence.toFixed(2)})
                </title>
              </rect>
            );
          })}
        </g>

        {/* Logo presence. */}
        <g>
          <rect
            x={0}
            y={trackTop.logo}
            width={1000}
            height={TRACK.logo}
            fill="var(--color-panel)"
          />
          {analysis.logo_intervals.map((interval) => (
            <rect
              key={interval.start}
              x={x(interval.start)}
              y={trackTop.logo}
              width={Math.max(x(interval.end) - x(interval.start), 0.5)}
              height={TRACK.logo}
              fill="var(--color-logo)"
              fillOpacity={0.75}
            />
          ))}
        </g>

        {/* Scene changes, drawn at their measured strength. */}
        <g>
          <rect
            x={0}
            y={trackTop.scene}
            width={1000}
            height={TRACK.scene}
            fill="var(--color-panel)"
          />
          {analysis.scene_changes.map((change) => {
            const strength = Math.min(change.strength / 120, 1);
            return (
              <line
                key={change.seconds}
                x1={x(change.seconds)}
                x2={x(change.seconds)}
                y1={trackTop.scene + TRACK.scene}
                y2={trackTop.scene + TRACK.scene * (1 - strength)}
                stroke="var(--color-scene)"
                strokeWidth={1}
                strokeOpacity={0.35 + strength * 0.65}
                vectorEffect="non-scaling-stroke"
              />
            );
          })}
        </g>

        {/* Silences. */}
        <g>
          <rect
            x={0}
            y={trackTop.silence}
            width={1000}
            height={TRACK.silence}
            fill="var(--color-panel)"
          />
          {analysis.silent_spans.map((span) => (
            <rect
              key={span.start}
              x={x(span.start)}
              y={trackTop.silence}
              width={Math.max(x(span.end) - x(span.start), 1)}
              height={TRACK.silence}
              fill="var(--color-silence)"
              fillOpacity={0.8}
            />
          ))}
        </g>

        {/* Scrub line. */}
        {hover !== null && (
          <line
            x1={x(hover)}
            x2={x(hover)}
            y1={0}
            y2={HEIGHT}
            stroke="var(--color-scene)"
            strokeWidth={1}
            strokeOpacity={0.6}
            vectorEffect="non-scaling-stroke"
            pointerEvents="none"
          />
        )}
      </svg>

      <figcaption className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-2">
        <Key colour="var(--color-programme)" label="Programme" />
        <Key colour="var(--color-commercial)" label="CM" />
        <Key colour="var(--color-logo)" label="Logo present" />
        <Key colour="var(--color-scene)" label="Scene change" />
        <Key colour="var(--color-silence)" label="Silence" />
        <span className="ml-auto tabular-nums text-ink-dim">
          {hover === null
            ? `${formatDuration(duration)} total`
            : formatDuration(hover)}
        </span>
      </figcaption>
    </figure>
  );
}

function Key({ colour, label }: { colour: string; label: string }) {
  return (
    <span className="flex items-center gap-2 text-[12px] text-ink-dim">
      <span
        className="inline-block h-2 w-4"
        style={{ background: colour }}
        aria-hidden="true"
      />
      {label}
    </span>
  );
}
