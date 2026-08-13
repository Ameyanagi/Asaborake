/**
 * What this machine can encode with.
 *
 * A profile whose encoder is missing from the ffmpeg build is shown rather
 * than hidden, because "why can I not pick NVENC" is the question this view
 * exists to answer.
 */

import { useEffect, useState } from "react";
import { api, type Health, type Profile } from "../lib/api";
import { Empty, Failure, Page } from "../components/shell";

export function Profiles() {
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void api
      .listProfiles()
      .then(setProfiles)
      .catch((cause: Error) => setError(cause.message));
    void api.health().then(setHealth).catch(() => {});
  }, []);

  return (
    <Page
      title="Profiles"
      detail={health ? `ffmpeg ${health.ffmpeg}` : undefined}
    >
      {error && <Failure message={error} />}

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
