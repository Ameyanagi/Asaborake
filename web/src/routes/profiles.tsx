/**
 * What this machine can encode with.
 *
 * A profile whose encoder is missing from the ffmpeg build is shown rather
 * than hidden, because "why can I not pick NVENC" is the question this view
 * exists to answer.
 */

import { useEffect, useState } from "react";
import { api, type Health, type Profile } from "../lib/api";
import { Action, Empty, Failure, Page } from "../components/shell";

export function Profiles() {
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [saved, setSaved] = useState<string | null>(null);

  const load = () => {
    void api
      .listProfiles()
      .then(setProfiles)
      .catch((cause: Error) => setError(cause.message));
    void api.health().then(setHealth).catch(() => {});
  };

  useEffect(load, []);

  const edit = (name: string) => {
    setError(null);
    void api
      .getProfile(name)
      .then((profile) => {
        setEditing(name);
        setDraft(profile.toml);
      })
      .catch((cause: Error) => setError(cause.message));
  };

  const save = () => {
    setError(null);
    void api
      .saveProfile(draft)
      .then((result) => {
        setEditing(null);
        setSaved(`Saved ${result.name}.`);
        setTimeout(() => setSaved(null), 3000);
        load();
      })
      // The engine parses before it writes, so a mistake comes back as a
      // message about the document rather than as a broken profile.
      .catch((cause: Error) => setError(cause.message));
  };

  return (
    <Page
      title="Profiles"
      detail={health ? `ffmpeg ${health.ffmpeg}` : undefined}
    >
      {error && <Failure message={error} />}
      {saved && (
        <p className="mx-6 mt-6 border-l-2 border-good bg-panel px-4 py-3 font-sans text-ink">
          {saved}
        </p>
      )}

      {editing && (
        <section className="border-b border-rule px-6 py-5">
          <h2 className="eyebrow mb-3">Editing {editing}</h2>
          {/* The TOML itself, because a profile *is* a TOML document: the
              thing the engine parses and the thing somebody would edit in a
              text editor. A form over it would be a second representation to
              drift from the first. */}
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            spellCheck={false}
            rows={18}
            className="w-full border border-rule-bright bg-panel px-3 py-2 text-[13px] leading-relaxed text-ink"
          />
          <div className="mt-4 flex flex-wrap items-center gap-4">
            <Action onClick={save}>Save</Action>
            <Action onClick={() => setEditing(null)}>Cancel</Action>
            <Action
              tone="alert"
              onClick={() => {
                void api
                  .forgetProfile(editing)
                  .then(() => {
                    setEditing(null);
                    setSaved(`Reverted ${editing}.`);
                    setTimeout(() => setSaved(null), 3000);
                    load();
                  })
                  .catch((cause: Error) => setError(cause.message));
              }}
            >
              Revert to shipped
            </Action>
            <span className="font-sans text-ink-dim">
              Changing the name saves it as a new profile rather than replacing
              this one.
            </span>
          </div>
        </section>
      )}

      {profiles?.length === 0 && (
        <Empty
          title="No profiles"
          detail="Asaborake ships four. If none are listed, the engine could not read them."
        />
      )}

      {profiles && profiles.length > 0 && (
        <div className="border-t border-rule">
          {profiles.map((profile) => (
            <div key={profile.name} className="rule-row px-6 py-4">
              <div className="flex items-baseline gap-4">
                <span
                  className="inline-block h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{
                    background: profile.available
                      ? "var(--color-good)"
                      : "var(--color-ink-faint)",
                  }}
                  aria-hidden="true"
                />
                <span className="w-32 shrink-0 text-ink">{profile.name}</span>
                <span className="w-28 shrink-0 text-ink-dim">
                  {profile.encoder}
                </span>
                <span className="w-12 shrink-0 text-ink-faint">
                  {profile.container}
                </span>
                {!profile.available && (
                  <span className="eyebrow" style={{ color: "var(--color-ink-faint)" }}>
                    encoder not in this ffmpeg build
                  </span>
                )}
              </div>
              <p className="mt-1.5 pl-[calc(0.375rem+1rem)] font-sans text-ink-dim">
                {profile.description}
              </p>
              <div className="mt-2 pl-[calc(0.375rem+1rem)]">
                <Action onClick={() => edit(profile.name)}>Edit</Action>
              </div>
            </div>
          ))}
        </div>
      )}

      {health && (
        <section className="px-6 py-5">
          <h2 className="eyebrow mb-3">Encoders in this build</h2>
          <div className="flex flex-wrap gap-x-8 gap-y-2">
            {Object.entries(health.encoders).map(([name, present]) => (
              <span
                key={name}
                className={present ? "text-ink" : "text-ink-faint line-through"}
              >
                {name}
              </span>
            ))}
          </div>
        </section>
      )}
    </Page>
  );
}
