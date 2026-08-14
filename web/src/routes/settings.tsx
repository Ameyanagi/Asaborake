/**
 * The decisions that apply to every recording, rather than to one.
 *
 * Two mechanisms, deliberately shown together because they overlap and reading
 * either alone would be misleading: a channel is the general case, a rule is
 * the particular one, and a matching rule wins. Seeing both on one screen is
 * the only way to answer "why did this recording get that profile".
 */

import { useEffect, useState } from "react";
import {
  api,
  type ChannelSettings,
  type Profile,
  type Rule,
} from "../lib/api";
import { Action, Failure, Page } from "../components/shell";

export function Settings() {
  const [channels, setChannels] = useState<Record<string, ChannelSettings>>({});
  const [rules, setRules] = useState<Rule[]>([]);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);

  const load = () => {
    void api.listChannels().then(setChannels).catch((cause: Error) => setError(cause.message));
    void api.listRules().then(setRules).catch((cause: Error) => setError(cause.message));
    void api.listProfiles().then(setProfiles).catch(() => {});
  };

  useEffect(load, []);

  const announce = (message: string) => {
    setSaved(message);
    setError(null);
    // Long enough to read, short enough not to become furniture.
    setTimeout(() => setSaved(null), 3000);
  };

  return (
    <Page title="Settings" detail="what applies to every recording">
      {error && <Failure message={error} />}
      {saved && (
        <p className="mx-6 mt-6 border-l-2 border-good bg-panel px-4 py-3 font-sans text-ink">
          {saved}
        </p>
      )}

      <Channels
        channels={channels}
        profiles={profiles}
        onChanged={(message) => {
          announce(message);
          load();
        }}
        onError={setError}
      />

      <Rules
        rules={rules}
        profiles={profiles}
        onChanged={(next, message) => {
          setRules(next);
          announce(message);
        }}
        onError={setError}
      />
    </Page>
  );
}

/** Per-channel treatment. */
function Channels({
  channels,
  profiles,
  onChanged,
  onError,
}: {
  channels: Record<string, ChannelSettings>;
  profiles: Profile[];
  onChanged: (message: string) => void;
  onError: (message: string) => void;
}) {
  const [id, setId] = useState("");

  const save = (channel: string, settings: ChannelSettings) => {
    void api
      .setChannel(channel, settings)
      .then(() => onChanged(`Saved ${channel}.`))
      .catch((cause: Error) => onError(cause.message));
  };

  const entries = Object.entries(channels);

  return (
    <section className="border-b border-rule px-6 py-6">
      <h2 className="eyebrow mb-1">Channels</h2>
      <p className="mb-5 max-w-2xl font-sans leading-relaxed text-ink-dim">
        A channel that carries no advertising should not be searched for
        commercial breaks — the search cannot find what is not there, but it can
        find something, and cutting a programme that had no advertisements in it
        is the worst thing this can do.
      </p>

      {entries.length === 0 ? (
        <p className="mb-5 text-ink-faint">Nothing configured; every channel is treated the same.</p>
      ) : (
        <div className="mb-5 border-t border-rule">
          {entries.map(([channel, settings]) => (
            <div key={channel} className="rule-row flex flex-wrap items-center gap-6 px-2 py-3">
              <span className="w-40 shrink-0 truncate text-ink">
                {settings.name ?? channel}
              </span>
              <span className="w-20 shrink-0 tabular-nums text-ink-faint">{channel}</span>

              <label className="flex items-center gap-2 text-ink-dim">
                <input
                  type="checkbox"
                  checked={settings.detect_commercials}
                  onChange={(event) =>
                    save(channel, {
                      ...settings,
                      detect_commercials: event.target.checked,
                    })
                  }
                />
                look for commercials
              </label>

              <label className="flex items-center gap-2 text-ink-dim">
                profile
                <select
                  value={settings.profile ?? ""}
                  onChange={(event) =>
                    save(channel, {
                      ...settings,
                      profile: event.target.value || null,
                    })
                  }
                  className="border border-rule-bright bg-panel px-2 py-1 text-ink"
                >
                  <option value="">whatever the job asked for</option>
                  {profiles.map((profile) => (
                    <option key={profile.name} value={profile.name} disabled={!profile.available}>
                      {profile.name}
                    </option>
                  ))}
                </select>
              </label>

              <Action
                tone="alert"
                onClick={() => {
                  void api
                    .forgetChannel(channel)
                    .then(() => onChanged(`Removed ${channel}.`))
                    .catch((cause: Error) => onError(cause.message));
                }}
              >
                Remove
              </Action>
            </div>
          ))}
        </div>
      )}

      <div className="flex flex-wrap items-end gap-3">
        <label className="flex flex-col gap-1.5">
          <span className="eyebrow">Channel id</span>
          <input
            value={id}
            onChange={(event) => setId(event.target.value)}
            placeholder="as EPGStation sends it"
            className="w-56 border border-rule-bright bg-panel px-3 py-1.5 text-ink placeholder:text-ink-faint"
          />
        </label>
        <Action
          disabled={!id.trim()}
          onClick={() => {
            save(id.trim(), {
              name: null,
              detect_commercials: true,
              profile: null,
            });
            setId("");
          }}
        >
          Add channel
        </Action>
      </div>
    </section>
  );
}

/** The auto-selection rules, in the order they are tried. */
function Rules({
  rules,
  profiles,
  onChanged,
  onError,
}: {
  rules: Rule[];
  profiles: Profile[];
  onChanged: (rules: Rule[], message: string) => void;
  onError: (message: string) => void;
}) {
  const commit = (next: Rule[], message: string) => {
    void api
      .replaceRules(next)
      .then(() => onChanged(next, message))
      .catch((cause: Error) => onError(cause.message));
  };

  const update = (index: number, patch: Partial<Rule>) => {
    const next = rules.map((rule, at) => (at === index ? { ...rule, ...patch } : rule));
    commit(next, "Saved the rules.");
  };

  /** Order is part of the meaning, so moving a rule is an edit. */
  const move = (index: number, by: number) => {
    const target = index + by;
    if (target < 0 || target >= rules.length) return;
    const next = [...rules];
    const [rule] = next.splice(index, 1);
    if (rule) next.splice(target, 0, rule);
    commit(next, "Reordered the rules.");
  };

  return (
    <section className="px-6 py-6">
      <h2 className="eyebrow mb-1">Rules</h2>
      <p className="mb-5 max-w-2xl font-sans leading-relaxed text-ink-dim">
        Tried from the top; the first one that matches wins and overrides the
        channel's own settings. A rule with no conditions matches everything,
        which makes it useful as the last entry and a mistake anywhere else.
      </p>

      {rules.length === 0 ? (
        <p className="mb-5 text-ink-faint">No rules; every recording is treated the same.</p>
      ) : (
        <div className="mb-5 border-t border-rule">
          {rules.map((rule, index) => (
            <div
              key={`${index}-${rule.name ?? ""}`}
              className="rule-row flex flex-wrap items-center gap-4 px-2 py-3"
            >
              <span className="w-6 shrink-0 tabular-nums text-ink-faint">{index + 1}</span>

              <input
                value={rule.name ?? ""}
                onChange={(event) => update(index, { name: event.target.value || null })}
                placeholder="what this is for"
                className="w-44 border border-rule-bright bg-panel px-2 py-1 text-ink placeholder:text-ink-faint"
              />
              <label className="flex items-center gap-2 text-ink-dim">
                title has
                <input
                  value={rule.title_contains ?? ""}
                  onChange={(event) =>
                    update(index, { title_contains: event.target.value || null })
                  }
                  className="w-32 border border-rule-bright bg-panel px-2 py-1 text-ink"
                />
              </label>
              <label className="flex items-center gap-2 text-ink-dim">
                channel
                <input
                  value={rule.channel_id ?? ""}
                  onChange={(event) => update(index, { channel_id: event.target.value || null })}
                  className="w-24 border border-rule-bright bg-panel px-2 py-1 text-ink"
                />
              </label>
              <label className="flex items-center gap-2 text-ink-dim">
                use
                <select
                  value={rule.profile ?? ""}
                  onChange={(event) => update(index, { profile: event.target.value || null })}
                  className="border border-rule-bright bg-panel px-2 py-1 text-ink"
                >
                  <option value="">no change</option>
                  {profiles.map((profile) => (
                    <option key={profile.name} value={profile.name} disabled={!profile.available}>
                      {profile.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="flex items-center gap-2 text-ink-dim">
                <input
                  type="checkbox"
                  checked={rule.detect_commercials !== false}
                  onChange={(event) =>
                    update(index, {
                      detect_commercials: event.target.checked ? null : false,
                    })
                  }
                />
                look for commercials
              </label>

              <div className="ml-auto flex gap-2">
                <Action disabled={index === 0} onClick={() => move(index, -1)}>
                  ↑
                </Action>
                <Action disabled={index === rules.length - 1} onClick={() => move(index, 1)}>
                  ↓
                </Action>
                <Action
                  tone="alert"
                  onClick={() =>
                    commit(
                      rules.filter((_, at) => at !== index),
                      "Removed the rule.",
                    )
                  }
                >
                  Remove
                </Action>
              </div>
            </div>
          ))}
        </div>
      )}

      <Action
        onClick={() =>
          commit(
            [
              ...rules,
              {
                name: "new rule",
                channel_id: null,
                title_contains: null,
                path_contains: null,
                min_height: null,
                profile: null,
                priority: null,
                detect_commercials: null,
              },
            ],
            "Added a rule.",
          )
        }
      >
        Add rule
      </Action>
    </section>
  );
}
