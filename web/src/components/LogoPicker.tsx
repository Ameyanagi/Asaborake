/**
 * Teaching the machine where a channel's logo is.
 *
 * This is the one screen that decides whether CM detection works at all.
 * Asaborake can find a logo unaided on clean material, but on real Japanese
 * broadcast the corner it should be watching is routinely occupied by a
 * permanent telop banner or an emergency overlay, and the automatic locator
 * picks the wrong thing. Someone looking at the picture never does.
 *
 * So: pick a recording, scrub to a moment where the logo is visible, drag a
 * box round it, scan. Amatsukaze has had this since the beginning and it is
 * why its detection works in practice.
 */

import { useEffect, useRef, useState } from "react";
import {
  api,
  formatDuration,
  type Rect,
  type Recording,
  type ScanResult,
  type SourceInfo,
} from "../lib/api";
import { Action } from "./shell";

/** How wide the frame is rendered. Big enough to aim a small logo inside. */
const FRAME_WIDTH = 960;

/** A box being drawn, in displayed pixels. */
interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function LogoPicker({ onLearned }: { onLearned: () => void }) {
  const [recordings, setRecordings] = useState<Recording[]>([]);
  const [path, setPath] = useState("");
  const [source, setSource] = useState<SourceInfo | null>(null);
  const [at, setAt] = useState(0);
  const [box, setBox] = useState<Box | null>(null);
  const [channelId, setChannelId] = useState("");
  const [name, setName] = useState("");
  const [scanning, setScanning] = useState(false);
  const [result, setResult] = useState<ScanResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const imageRef = useRef<HTMLImageElement>(null);
  const dragStart = useRef<{ x: number; y: number } | null>(null);

  useEffect(() => {
    void api
      .listRecordings()
      .then(setRecordings)
      .catch((cause: Error) => setError(cause.message));
  }, []);

  // A new recording invalidates everything measured against the old one.
  useEffect(() => {
    if (!path) return;
    setSource(null);
    setBox(null);
    setResult(null);
    setAt(0);
    void api
      .probeRecording(path)
      .then((info) => {
        setSource(info);
        // A quarter of the way in, rather than the first frame: recordings
        // start on a title card or the tail of the previous programme, and
        // the logo is often not up yet.
        setAt(Math.floor((info.duration_seconds ?? 0) / 4));
      })
      .catch((cause: Error) => setError(cause.message));
  }, [path]);

  /** Where a pointer event landed, in displayed pixels. */
  const pointAt = (event: React.PointerEvent) => {
    const bounds = imageRef.current?.getBoundingClientRect();
    if (!bounds) return null;
    return {
      x: Math.round(event.clientX - bounds.left),
      y: Math.round(event.clientY - bounds.top),
    };
  };

  const onPointerDown = (event: React.PointerEvent) => {
    const point = pointAt(event);
    if (!point) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    dragStart.current = point;
    setBox({ ...point, width: 0, height: 0 });
    setResult(null);
  };

  const onPointerMove = (event: React.PointerEvent) => {
    const start = dragStart.current;
    const point = pointAt(event);
    if (!start || !point) return;
    setBox({
      x: Math.min(start.x, point.x),
      y: Math.min(start.y, point.y),
      width: Math.abs(point.x - start.x),
      height: Math.abs(point.y - start.y),
    });
  };

  const onPointerUp = () => {
    dragStart.current = null;
  };

  /**
   * The drawn box in source pixels, which is what the scanner works in.
   *
   * The picture on screen is scaled, and for Japanese HD it is also
   * un-squashed from 1440x1080 to 16:9 — so the two axes have different
   * scale factors and each has to be converted on its own.
   */
  const sourceRect = (): Rect | null => {
    const element = imageRef.current;
    if (!element || !box || !source || box.width < 4 || box.height < 4) {
      return null;
    }
    const scaleX = source.width / element.clientWidth;
    const scaleY = source.height / element.clientHeight;
    return {
      x: Math.round(box.x * scaleX),
      y: Math.round(box.y * scaleY),
      width: Math.round(box.width * scaleX),
      height: Math.round(box.height * scaleY),
    };
  };

  const rect = sourceRect();

  const scan = () => {
    if (!rect) return;
    setScanning(true);
    setError(null);
    setResult(null);
    void api
      .scanLogo({
        path,
        rect,
        ...(channelId ? { channel_id: channelId } : {}),
        ...(name ? { name } : {}),
      })
      .then((outcome) => {
        setResult(outcome);
        if (outcome.learned) onLearned();
      })
      .catch((cause: Error) => setError(cause.message))
      .finally(() => setScanning(false));
  };

  const duration = source?.duration_seconds ?? 0;

  return (
    <section className="border-b border-rule px-6 py-6">
      <h2 className="eyebrow mb-1">Teach a logo</h2>
      <p className="mb-5 max-w-2xl font-sans leading-relaxed text-ink-dim">
        Pick a recording, find a moment where the station logo is showing, and
        drag a box round it. Asaborake learns the logo from the whole recording
        and reuses it for every job on that channel.
      </p>

      <div className="mb-5 flex flex-wrap items-end gap-4">
        <label className="flex flex-col gap-1.5">
          <span className="eyebrow">Recording</span>
          <select
            value={path}
            onChange={(event) => setPath(event.target.value)}
            className="min-w-80 border border-rule-bright bg-panel px-3 py-1.5 text-ink"
          >
            <option value="">Choose a recording…</option>
            {recordings.map((recording) => (
              <option key={recording.path} value={recording.path}>
                {recording.name}
              </option>
            ))}
          </select>
        </label>

        <label className="flex flex-col gap-1.5">
          <span className="eyebrow">Channel id</span>
          <input
            value={channelId}
            onChange={(event) => setChannelId(event.target.value)}
            placeholder="as EPGStation sends it"
            className="w-56 border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
          />
        </label>

        <label className="flex flex-col gap-1.5">
          <span className="eyebrow">Name</span>
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="日本テレビ"
            className="w-56 border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
          />
        </label>
      </div>

      {recordings.length === 0 && !error && (
        <p className="font-sans text-ink-dim">
          No recordings to show. Set <code>recording_dirs</code> in the engine
          configuration to the directory your recordings are written to.
        </p>
      )}

      {error && (
        <p className="mb-4 border-l-2 border-alert bg-panel px-4 py-3 font-sans text-ink">
          {error}
        </p>
      )}

      {source && (
        <>
          <div
            className="relative inline-block cursor-crosshair select-none border border-rule"
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
          >
            <img
              ref={imageRef}
              src={api.frameUrl(path, at, FRAME_WIDTH)}
              alt={`Frame at ${formatDuration(at)}`}
              draggable={false}
              className="block max-w-full"
            />
            {box && box.width > 2 && box.height > 2 && (
              <div
                className="pointer-events-none absolute border-2"
                style={{
                  left: box.x,
                  top: box.y,
                  width: box.width,
                  height: box.height,
                  borderColor: "var(--color-programme)",
                  boxShadow: "0 0 0 9999px rgba(0,0,0,0.45)",
                }}
              />
            )}
          </div>

          <div className="mt-4 flex flex-wrap items-center gap-5">
            <input
              type="range"
              min={0}
              max={Math.max(duration - 1, 1)}
              step={1}
              value={at}
              onChange={(event) => setAt(Number(event.target.value))}
              className="w-96 max-w-full accent-[var(--color-programme)]"
              aria-label="Position in the recording"
            />
            <span className="tabular-nums text-ink-dim">
              {formatDuration(at)} of {formatDuration(duration)}
            </span>
            <span className="tabular-nums text-ink-faint">
              {source.width}×{source.height}
              {source.interlaced && " interlaced"}
            </span>
          </div>

          <div className="mt-5 flex flex-wrap items-center gap-5">
            <Action onClick={scan} disabled={!rect || scanning}>
              {scanning ? "Scanning…" : "Scan this box"}
            </Action>
            {rect ? (
              <span className="tabular-nums text-ink-dim">
                {rect.width}×{rect.height} at {rect.x},{rect.y}
              </span>
            ) : (
              <span className="text-ink-faint">
                Drag a box over the logo to scan it
              </span>
            )}
            {scanning && (
              <span className="font-sans text-ink-dim">
                Reading the whole recording; this takes a minute or two.
              </span>
            )}
          </div>
        </>
      )}

      {result && <Outcome result={result} />}
    </section>
  );
}

/** What the scan found, or why it found nothing. */
function Outcome({ result }: { result: ScanResult }) {
  if (!result.learned) {
    return (
      <div className="mt-5 flex flex-wrap items-start gap-6 border-l-2 border-alert bg-panel px-4 py-3">
        {/* The rejected fit. A recognisable logo that missed the bar and a
            rectangle full of noise look nothing alike, and no number conveys
            the difference as fast as seeing it does. */}
        <div
          className="flex h-20 w-36 shrink-0 items-center justify-center border border-rule"
          style={{
            backgroundImage:
              "linear-gradient(45deg, #1a2430 25%, transparent 25%), linear-gradient(-45deg, #1a2430 25%, transparent 25%), linear-gradient(45deg, transparent 75%, #1a2430 75%), linear-gradient(-45deg, transparent 75%, #1a2430 75%)",
            backgroundSize: "8px 8px",
            backgroundPosition: "0 0, 0 4px, 4px -4px, -4px 0px",
          }}
        >
          {result.preview ? (
            <img
              src={result.preview}
              alt="What the box caught, which was rejected"
              className="max-h-full max-w-full object-contain"
            />
          ) : (
            <span className="eyebrow">nothing</span>
          )}
        </div>

        <div className="min-w-0 flex-1">
          <div className="eyebrow" style={{ color: "var(--color-alert)" }}>
            nothing usable
          </div>
          <p className="mt-1.5 max-w-2xl font-sans leading-relaxed text-ink">
            {result.reason}
          </p>
          <div className="mt-2 flex flex-wrap gap-x-5 gap-y-1 tabular-nums text-ink-dim">
            <span>{result.frames_used} usable frames</span>
            <span>background range {result.background_spread} of 255</span>
            <span>{result.strong_pixels} solid pixels</span>
            <span>mean opacity {result.mean_alpha.toFixed(3)}</span>
          </div>
          <p className="mt-2 max-w-2xl font-sans leading-relaxed text-ink-dim">
            If the preview looks like your logo, the box is right and the bar
            was too strict for this material. If it looks like nothing, the box
            is in the wrong place.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="mt-5 flex items-center gap-6 border-l-2 border-good bg-panel px-4 py-3">
      {result.preview && (
        <div
          className="flex h-20 w-36 shrink-0 items-center justify-center border border-rule"
          style={{
            backgroundImage:
              "linear-gradient(45deg, #1a2430 25%, transparent 25%), linear-gradient(-45deg, #1a2430 25%, transparent 25%), linear-gradient(45deg, transparent 75%, #1a2430 75%), linear-gradient(-45deg, transparent 75%, #1a2430 75%)",
            backgroundSize: "8px 8px",
            backgroundPosition: "0 0, 0 4px, 4px -4px, -4px 0px",
          }}
        >
          <img
            src={result.preview}
            alt={`Learned logo for ${result.name}`}
            className="max-h-full max-w-full object-contain"
          />
        </div>
      )}
      <div>
        <div className="eyebrow" style={{ color: "var(--color-good)" }}>
          learned and saved
        </div>
        <p className="mt-1.5 font-sans text-ink">
          {result.name} — opacity {result.mean_alpha.toFixed(2)}, fitted from{" "}
          {result.frames_used} frames.
        </p>
        <p className="mt-1 font-sans text-ink-dim">
          Check the preview: it is what the detector matches against, so a bad
          fit looks wrong here before it costs you a recording.
        </p>
      </div>
    </div>
  );
}
