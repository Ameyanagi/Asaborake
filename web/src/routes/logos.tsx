/**
 * The logos the machine has learned.
 *
 * The preview is the point. A learned logo is rendered with its measured
 * opacity as the alpha channel, so what you see here is literally what the
 * detector matches against — and a bad fit looks wrong immediately, which no
 * number in a table would tell you.
 */

import { useEffect, useState } from "react";
import { api, type Logo } from "../lib/api";
import { LogoPicker } from "../components/LogoPicker";
import { Action, Empty, Failure, Page } from "../components/shell";

export function Logos() {
  const [logos, setLogos] = useState<Logo[] | null>(null);
  const [without, setWithout] = useState<string[]>([]);
  const [channel, setChannel] = useState("");
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    void api
      .listLogos()
      .then((answer) => {
        setLogos(answer.logos);
        setWithout(answer.channels_without_logos);
      })
      .catch((cause: Error) => setError(cause.message));
  };

  useEffect(load, []);

  const forget = (logo: Logo) => {
    if (!logo.channel_id) return;
    void api
      .forgetLogo(logo.channel_id, logo.source_width, logo.source_height)
      .then(load)
      .catch((cause: Error) => setError(cause.message));
  };

  return (
    <Page
      title="Logos"
      detail="one per channel and picture size"
      aside={
        logos && logos.length > 0 ? (
          <span className="tabular-nums text-ink-dim">{logos.length} learned</span>
        ) : undefined
      }
    >
      {error && <Failure message={error} />}

      <LogoPicker onLearned={load} />

      {logos?.length === 0 && (
        <Empty
          title="No logos learned yet"
          detail="Teach one above, or let a job learn it: Asaborake tries to find a channel's logo the first time it transcodes a recording from it. Teaching is the reliable route — on real broadcast the corner it should be watching is often covered by a telop banner, and it aims at that instead."
        />
      )}

      <section className="border-b border-rule px-6 py-5">
        <h2 className="eyebrow mb-1">Channels with no logo</h2>
        <p className="mb-4 max-w-2xl font-sans leading-relaxed text-ink-dim">
          Some channels simply have no watermark. Saying so here stops every
          recording from one spending three extra decoding passes discovering
          that again, and stops its jobs waiting for a logo that is not coming.
        </p>

        <div className="mb-4 flex flex-wrap items-end gap-3">
          <input
            value={channel}
            onChange={(event) => setChannel(event.target.value)}
            placeholder="channel id"
            className="w-56 border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
          />
          <Action
            disabled={!channel.trim()}
            onClick={() => {
              void api
                .markNoLogo(channel.trim())
                .then(() => {
                  setChannel("");
                  load();
                })
                .catch((cause: Error) => setError(cause.message));
            }}
          >
            This channel has no logo
          </Action>
        </div>

        {without.length === 0 ? (
          <p className="text-ink-faint">None marked.</p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {without.map((id) => (
              <li key={id}>
                <button
                  type="button"
                  title="Look for a logo on this channel again"
                  onClick={() => {
                    void api
                      .clearNoLogo(id)
                      .then(load)
                      .catch((cause: Error) => setError(cause.message));
                  }}
                  className="border border-rule-bright px-2.5 py-1 text-ink-dim transition-colors hover:border-alert hover:text-alert"
                >
                  {id} ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {logos && logos.length > 0 && (
        <div className="border-t border-rule">
          {logos.map((logo) => (
            <div
              key={`${logo.channel_id}-${logo.source_width}x${logo.source_height}`}
              className="rule-row flex items-center gap-6 px-6 py-4"
            >
              {/* Checkerboard behind the preview, so a semi-transparent logo
                  reads as semi-transparent rather than as dark grey. */}
              <div
                className="flex h-16 w-32 shrink-0 items-center justify-center border border-rule"
                style={{
                  backgroundImage:
                    "linear-gradient(45deg, #1a2430 25%, transparent 25%), linear-gradient(-45deg, #1a2430 25%, transparent 25%), linear-gradient(45deg, transparent 75%, #1a2430 75%), linear-gradient(-45deg, transparent 75%, #1a2430 75%)",
                  backgroundSize: "8px 8px",
                  backgroundPosition: "0 0, 0 4px, 4px -4px, -4px 0px",
                }}
              >
                {logo.preview ? (
                  <img
                    src={logo.preview}
                    alt={`Learned logo for ${logo.name}`}
                    className="max-h-full max-w-full object-contain"
                  />
                ) : (
                  <span className="eyebrow">no preview</span>
                )}
              </div>

              <div className="min-w-0 flex-1">
                <div className="truncate text-ink">{logo.name}</div>
                <div className="mt-1 flex flex-wrap gap-x-5 gap-y-1 tabular-nums text-ink-dim">
                  <span>channel {logo.channel_id ?? "unknown"}</span>
                  <span>
                    {logo.source_width}×{logo.source_height}
                  </span>
                  <span>
                    at {logo.rect.x},{logo.rect.y} · {logo.rect.width}×
                    {logo.rect.height}
                  </span>
                  <span>opacity {logo.mean_alpha.toFixed(2)}</span>
                  <span>{logo.frames_used} frames</span>
                </div>
              </div>

              <Action tone="alert" onClick={() => forget(logo)}>
                Forget
              </Action>
            </div>
          ))}
        </div>
      )}
    </Page>
  );
}
