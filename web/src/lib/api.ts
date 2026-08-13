/**
 * The engine's API, as the browser sees it.
 *
 * The types mirror the Rust structs they come from. They are written by hand
 * rather than generated because they are small and stable, and a generator in
 * the build would have to run against a live engine.
 */

/** Where a job has got to. */
export type JobStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

/** One job in the queue. */
export interface Job {
  id: string;
  input: string;
  output: string;
  profile: string;
  title: string | null;
  channel_id: string | null;
  channel_name: string | null;
  status: JobStatus;
  priority: number;
  /** Completion, 0 to 1. */
  progress: number;
  message: string;
  error: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

/** A line a job logged. */
export interface JobEvent {
  id: number;
  at: string;
  level: string;
  message: string;
}

/** A rectangle within a frame, in pixels. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A learned logo, as the list presents it. */
export interface Logo {
  name: string;
  channel_id: string | null;
  source_width: number;
  source_height: number;
  rect: Rect;
  mean_alpha: number;
  frames_used: number;
  /** PNG data URI of the learned opacity and colour. */
  preview: string | null;
}

/** An encoding profile. */
export interface Profile {
  name: string;
  description: string;
  container: string;
  encoder: string;
  /** Whether this ffmpeg build can actually run it. */
  available: boolean;
}

/** A span during which the logo was present. */
export interface LogoInterval {
  start: number;
  end: number;
}

/** A detected cut. */
export interface SceneChange {
  seconds: number;
  strength: number;
}

/** A stretch of quiet audio. */
export interface SilentSpan {
  start: number;
  end: number;
}

/** What the analysis pass found. */
export interface Analysis {
  duration_seconds: number;
  seconds_per_frame: number;
  logo: {
    rect: Rect;
    mean_alpha: number;
    frames_used: number;
    from_store: boolean;
  } | null;
  logo_intervals: LogoInterval[];
  logo_track: { seconds_per_frame: number; scores: number[] } | null;
  scene_changes: SceneChange[];
  silent_spans: SilentSpan[];
}

/** What a stretch of the recording was judged to be. */
export type SegmentKind = "programme" | "commercial";

/** One labelled stretch. */
export interface Segment {
  start: number;
  end: number;
  kind: SegmentKind;
  confidence: number;
}

/** The segmenter's answer. */
export interface CutPlan {
  segments: Segment[];
  keep: { start: number; end: number }[];
  confidence: number;
  decision: "cut" | "keep_all";
  reason: string;
}

/**
 * What the source recording contained, and what was wrong with it.
 *
 * Only transport streams carry any of this — an MP4 has no continuity counters
 * to be discontinuous and nothing to be scrambled — so a job from any other
 * source has none.
 */
export interface Diagnostics {
  duration_seconds: number;
  video: string | null;
  audio: string[];
  has_captions: boolean;
  format_changes: number[];
  dropped_packets: number;
  scrambled_packets: number;
  error_packets: number;
  total_packets: number;
  /** Set when the recording carries two languages on one stream's channels. */
  dual_mono: { main: string | null; sub: string | null } | null;
  warnings: string[];
}

/** The engine's health, and what it can encode with. */
export interface Health {
  status: string;
  version: string;
  ffmpeg: string;
  encoders: Record<string, boolean>;
  logo_store: boolean;
}

/** An update pushed over the event stream. */
export type Update =
  | { type: "job"; job: Job }
  | { type: "log"; job_id: string; message: string };

/** Thrown when the engine answers with an error. */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/v1${path}`, {
    headers: { "content-type": "application/json" },
    ...init,
  });

  if (!response.ok) {
    // The engine and the web server both answer with `{ error }`, so one
    // reading covers a failure from either.
    const detail = await response
      .json()
      .then((body: { error?: string }) => body.error)
      .catch(() => undefined);
    throw new ApiError(
      detail ?? `request failed with ${response.status}`,
      response.status,
    );
  }

  return response.json() as Promise<T>;
}

export const api = {
  health: () => request<Health>("/health"),

  listJobs: (limit = 100) => request<Job[]>(`/jobs?limit=${limit}`),
  getJob: (id: string) => request<Job>(`/jobs/${id}`),
  jobEvents: (id: string, after = 0) =>
    request<JobEvent[]>(`/jobs/${id}/events?after=${after}`),
  jobAnalysis: (id: string) =>
    request<{
      analysis: Analysis | null;
      plan: CutPlan | null;
      diagnostics: Diagnostics | null;
    }>(`/jobs/${id}/analysis`),

  submitJob: (job: {
    input: string;
    output: string;
    profile: string;
    title?: string;
    channel_id?: string;
    channel_name?: string;
    priority?: number;
  }) =>
    request<{ id: string }>("/jobs", {
      method: "POST",
      body: JSON.stringify(job),
    }),

  cancelJob: (id: string) =>
    request<{ cancelled: boolean }>(`/jobs/${id}/cancel`, { method: "POST" }),
  retryJob: (id: string) =>
    request<{ id: string }>(`/jobs/${id}/retry`, { method: "POST" }),

  listLogos: () => request<Logo[]>("/logos"),
  forgetLogo: (channel: string, width: number, height: number) =>
    request<{ removed: boolean }>(`/logos/${channel}/${width}/${height}`, {
      method: "DELETE",
    }),

  listProfiles: () => request<Profile[]>("/profiles"),

  listRecordings: () => request<Recording[]>("/recordings"),

  probeRecording: (path: string) =>
    request<SourceInfo>(`/recordings/probe?path=${encodeURIComponent(path)}`),

  /**
   * The URL of one frame, for an `<img src>`.
   *
   * A URL rather than a fetch because the browser's own image cache is what
   * makes scrubbing back and forth feel immediate.
   */
  frameUrl: (path: string, at: number, width: number) =>
    `/api/v1/frame?path=${encodeURIComponent(path)}&at=${at.toFixed(2)}&width=${width}`,

  scanLogo: (body: {
    path: string;
    rect: Rect;
    channel_id?: string;
    name?: string;
  }) => request<ScanResult>("/logos/scan", {
    method: "POST",
    body: JSON.stringify(body),
  }),
};

/** A recording the logo tool may read. */
export interface Recording {
  path: string;
  name: string;
  size: number;
}

/** What a recording is, in the terms the logo tool needs. */
export interface SourceInfo {
  duration_seconds: number | null;
  width: number;
  height: number;
  fps: number;
  interlaced: boolean;
}

/** What came of scanning a rectangle. */
export type ScanResult =
  | { learned: false; reason: string }
  | {
      learned: true;
      name: string;
      channel_id: string | null;
      source_width: number;
      source_height: number;
      rect: Rect;
      mean_alpha: number;
      frames_used: number;
      preview: string | null;
    };

/**
 * Subscribe to live updates.
 *
 * Returns a function that closes the stream. `EventSource` reconnects on its
 * own, which is what makes an engine restart invisible to the page.
 */
export function subscribe(onUpdate: (update: Update) => void): () => void {
  const source = new EventSource("/api/v1/events");

  source.onmessage = (message) => {
    try {
      onUpdate(JSON.parse(message.data) as Update);
    } catch {
      // A malformed frame is not worth taking the stream down for.
    }
  };

  return () => source.close();
}

/** Format a duration in seconds as `m:ss`, or `h:mm:ss` past an hour. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const whole = Math.round(seconds);
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = whole % 60;
  const pad = (value: number) => String(value).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** A short, human description of when something happened. */
export function formatWhen(iso: string | null): string {
  if (!iso) return "—";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "—";

  const seconds = (Date.now() - at.getTime()) / 1000;
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return at.toLocaleDateString();
}
