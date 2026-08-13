/**
 * Putting a recording into the queue by hand.
 *
 * Everything else that reaches the queue comes from EPGStation, which is the
 * point — but there is always the recording that was made before Asaborake
 * existed, or the one whose job failed and needs different settings. Without
 * this the only way to queue one is curl.
 */

import { useEffect, useState } from "react";
import { api, type Profile, type Recording } from "../lib/api";
import { Action } from "./shell";

/** Turn a source path into a sensible output path. */
function suggestOutput(path: string, container: string): string {
  const withoutExtension = path.replace(/\.[^./]+$/, "");
  return `${withoutExtension}-cut.${container}`;
}

export function SubmitJob({ onSubmitted }: { onSubmitted: () => void }) {
  const [open, setOpen] = useState(false);
  const [recordings, setRecordings] = useState<Recording[]>([]);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [input, setInput] = useState("");
  const [output, setOutput] = useState("");
  const [profile, setProfile] = useState("");
  const [channelId, setChannelId] = useState("");
  const [title, setTitle] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    void api.listRecordings().then(setRecordings).catch(() => {});
    void api
      .listProfiles()
      .then((loaded) => {
        setProfiles(loaded);
        // A profile this ffmpeg cannot run would fail the job immediately, so
        // it is not what the form should open on.
        setProfile((current) => current || (loaded.find((p) => p.available)?.name ?? ""));
      })
      .catch(() => {});
  }, [open]);

  const chosen = profiles.find((p) => p.name === profile);

  /** Filling in the source fills in the rest, because it usually should. */
  const pickInput = (path: string) => {
    setInput(path);
    setOutput(suggestOutput(path, chosen?.container === "mkv" ? "mkv" : "mp4"));
    // EPGStation names recordings after the programme; the file name is the
    // best guess available and is easier to correct than to type.
    const name = path.split("/").pop() ?? "";
    setTitle(name.replace(/\.[^./]+$/, ""));
  };

  const submit = () => {
    setBusy(true);
    setError(null);
    void api
      .submitJob({
        input,
        output,
        profile,
        ...(title ? { title } : {}),
        ...(channelId ? { channel_id: channelId } : {}),
      })
      .then(() => {
        setOpen(false);
        setInput("");
        setOutput("");
        onSubmitted();
      })
      .catch((cause: Error) => setError(cause.message))
      .finally(() => setBusy(false));
  };

  if (!open) {
    return (
      <Action onClick={() => setOpen(true)}>Queue a recording</Action>
    );
  }

  return (
    <section className="border-b border-rule px-6 py-5">
      <h2 className="eyebrow mb-4">Queue a recording</h2>

      <div className="flex flex-wrap items-end gap-4">
        <label className="flex flex-col gap-1.5">
          <span className="eyebrow">Recording</span>
          <select
            value={input}
            onChange={(event) => pickInput(event.target.value)}
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
          <span className="eyebrow">Profile</span>
          <select
            value={profile}
            onChange={(event) => setProfile(event.target.value)}
            className="border border-rule-bright bg-panel px-3 py-1.5 text-ink"
          >
            {profiles.map((option) => (
              <option key={option.name} value={option.name} disabled={!option.available}>
                {option.name}
                {option.available ? "" : " (not available)"}
              </option>
            ))}
          </select>
        </label>

        <label className="flex flex-col gap-1.5">
          <span className="eyebrow">Channel id</span>
          <input
            value={channelId}
            onChange={(event) => setChannelId(event.target.value)}
            placeholder="finds the logo"
            className="w-44 border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
          />
        </label>
      </div>

      <label className="mt-4 flex flex-col gap-1.5">
        <span className="eyebrow">Write to</span>
        <input
          value={output}
          onChange={(event) => setOutput(event.target.value)}
          placeholder="/recordings/…"
          className="w-full max-w-2xl border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
        />
      </label>

      {error && (
        <p className="mt-4 border-l-2 border-alert bg-panel px-4 py-3 font-sans text-ink">
          {error}
        </p>
      )}

      <div className="mt-5 flex items-center gap-4">
        <Action onClick={submit} disabled={!input || !output || !profile || busy}>
          {busy ? "Queueing…" : "Queue it"}
        </Action>
        <Action onClick={() => setOpen(false)}>Cancel</Action>
        {recordings.length === 0 && (
          <span className="text-ink-faint">
            No recordings to choose from — set <code>recording_dirs</code> in
            the engine configuration.
          </span>
        )}
      </div>
    </section>
  );
}
