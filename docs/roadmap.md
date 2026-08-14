# What Amatsukaze does that Asaborake does not

Asaborake reproduces Amatsukaze's *detection* — the logo model, the flat-frame
learning, the correlation scoring — and wraps it in a web UI. What it does not
yet reproduce is most of what makes Amatsukaze usable on a real recording box.

This is the gap, and what I would do about it, in the order I would do it.

It comes from reading the source rather than the README: `AmatsukazeGUI/`
(what the operator can do), `AmatsukazeServer/` (what runs unattended), and
`Amatsukaze/` (what actually happens to a recording). File references are to
`reference/Amatsukaze/`, which `make reference` clones.

---

## The gap, honestly

### Asaborake silently loses things Amatsukaze keeps

This is the worst category, because nothing announces it. A recording goes
through and comes out smaller than it should be.

| | Amatsukaze | Asaborake |
| --- | --- | --- |
| **ARIB captions** | Extracted to SRT and ASS, up to 8 language tracks, re-timed across every cut, muxed as tracks (MKV) or written alongside (MP4) | **Dropped entirely** |
| **Dual-mono audio** | Split losslessly into two AAC tracks — bilingual programmes keep both languages | Downmixed to one, second language gone |
| **Extra audio streams** | Every audio ES becomes a track | Only stream 0 survives |
| **Audio coding** | AAC copied byte-for-byte from the TS by default | Always re-encoded, always lossy |
| **Format changes** | Output splits so each file holds one video format | Detected by `asaborake probe`, then ignored — the encode runs anyway |
| **A/V sync** | Audio rebuilt frame by frame against the video timeline, with drift statistics reported | Left to ffmpeg |

The audio ones are the most embarrassing: a bilingual programme loses a
language, and every recording is re-encoded when it could have been copied.

### Detection cannot be made to work by hand

| | Amatsukaze | Asaborake |
| --- | --- | --- |
| **Logo creation** | A dedicated window: scrub to any frame, drag a rectangle over the logo, scan, preview the extracted logo against a background slider, adopt it | Nothing. Automatic location only, and `--logo-rect` is blind — you cannot see what you are aiming at |
| **Per-channel logo rules** | Several logos per channel, each with a validity date range; an explicit "no logo" entry for channels that have none | One logo per channel and picture size |
| **Blocked rather than wrong** | A job with no usable logo sits in `LogoPending` until one exists | Proceeds without a logo and keeps the recording whole |
| **Logo removal** | `AMTEraseLogo` erases the logo from the output | Never — Asaborake only detects |

Today's test on real broadcast failed at exactly this point. The 日テレ
watermark was visible in the frame and I could not aim at it, because there is
no way to see what the rectangle covers.

### It cannot be left alone

| | Amatsukaze | Asaborake |
| --- | --- | --- |
| **Profile auto-selection** | Rules matching channel, ARIB genre, filename, video size or a script-set tag choose the profile and priority per recording | One profile, chosen by hand |
| **Per-channel settings** | Per-channel CM options, disable CM detection entirely for channels that never carry ads | None |
| **Run hours** | An hour-by-hour schedule; outside it the queue stops, and optionally running encoders suspend | Runs whenever |
| **When finished** | Sleep, hibernate or shut down, with a cancellable countdown | Nothing |
| **User hooks** | Scripts at add / pre-encode / post-encode with the programme's metadata in the environment, able to set tags, output directory, priority, or cancel | None |
| **Output naming** | Renaming from programme metadata, genre subfolders, duplicate handling | Whatever path the caller passed |
| **Housekeeping** | Source moved to `succeeded/`/`failed/`, duplicate submissions suppressed, disk space monitored, network sources hash-verified | None |

### The UI is a viewer, not a console

Asaborake's web UI has exactly three write actions: cancel a job, retry a job,
forget a logo. Amatsukaze's queue alone has drag-and-drop adding, search and
filtering across six fields, state and priority and date-range filters,
drag-reordering, inline priority editing, force-start, duplicate, re-apply
profile, delete-with-source, and batch operations over a multi-selection.

It also has a full profile editor, an auto-select rule builder, a per-channel
settings panel, a sortable history browser with CSV export and per-job
statistics (encode speed, compression ratio, average and maximum audio drift),
a DRCS glyph mapping panel, a disk-space panel, and a live console per encoder.

### One thing Asaborake already does better

**Amatsukaze has no manual CM editor.** Detection is entirely automatic; the
only override is dropping a `.trim.avs` file next to the source, and there is
no UI to produce one. Asaborake's timeline already draws the evidence and the
decision together. Making it editable would put Asaborake ahead, not level.

---

## What I would do, in order

The ordering principle: stop losing data, then make detection workable, then
make it unattended, then make it comfortable. Sizes are relative — S is a day
or so, M a few days, L a week or more.

### Stage 1 — stop losing things (highest value, least glamorous)

1. ~~**Keep every audio track, and copy rather than re-encode.**~~ **Done.**
   Every stream is a track and carries its language; audio is copied byte for
   byte whenever nothing is being cut. Amatsukaze goes further and reassembles
   AAC frames losslessly *across* cuts, which needs frame-level muxing rather
   than a filter graph and is not attempted.
2. ~~**Dual-mono handling.**~~ **Done**, though not where the note expected to
   find it. `probe`'s dual-mono guess was wrong: ARIB carries a bilingual
   programme as one stream in "1/0 + 1/0 mode", which ffprobe cannot tell from
   stereo. The signal is the audio component descriptor, and it lives in the
   *event* table rather than the program map. Read from there, the stream is
   split into two mono tracks tagged with their own languages.
3. ~~**ARIB captions to SRT and ASS.**~~ **SRT done.** A B24 decoder was
   written rather than shelled out to, and it needs no character table: ARIB's
   kanji set is JIS X 0208 in EUC-JP's arrangement with the high bit clear.
   Captions are re-timed through `chapters.rs` and written beside every
   output. ASS is still missing, and with it the positioning and colour; so is
   the superimpose stream that carries emergency crawls.
4. ~~**Split output on format change.**~~ **Done**, but not by acting on the
   change points in time — neither ffmpeg's filter clock nor its timestamps
   survive a picture-size change, and both were tried. The scan now records
   the *byte offset* of each change, and each part is copied out as its own
   transport stream before being encoded. Analysis still runs over the whole
   recording and cannot see past the change, so a size-changing recording
   that is also cut takes its later cut points from an analysis that never
   saw them.
5. ~~**Report what happened.**~~ **Done.** The stream inventory, drop and
   scramble counters, and the sentences they imply are kept with the job —
   recorded when it *starts*, so a job that fails still says what it was
   working from. Audio drift statistics are still missing; they need a
   PTS-versus-sample-count comparison the scan does not yet make.

### Stage 2 — make the logo workable (unblocks the core value)

6. ~~**A logo tool in the web UI.**~~ **Built.** Pick a recording, scrub, drag
   a box over the logo, scan, see the result previewed. Building it exposed
   that a fit of nothing at all was being stored as a logo and reused for
   every future recording on the channel, which is now refused. No good logo
   has yet been learned from real broadcast: the one watermark available to
   aim at is too faint to fit. The background slider is still missing.
7. **Per-channel logo rules.** The explicit "this channel has no logo" entry
   is ~~done~~: marking a channel skips both location passes outright and stops
   its jobs waiting for something that is not coming. Several logos per channel
   with validity date ranges is still missing. **M**
8. ~~**Block rather than guess.**~~ **Done.** `on_low_confidence = "block"`
   holds a job in a `blocked` state before the encode rather than after it,
   with a message naming what it needs. Deliberately not coloured as a
   failure. Off by default.
9. **Amatsukaze `.lgd` import.** Existing logo packs are the fastest route to
   working detection, and the format is documented in `LogoScan.hpp`. **S**

### Stage 3 — leave it alone overnight

10. **Profile auto-selection.** Rules over channel, genre, filename and video
    size choosing profile and priority. Needs ARIB genre parsing, which
    `asaborake-ts` does not do yet. **M**
11. ~~**Per-channel settings.**~~ **Done.** A channel can be told it carries no
    commercials — which skips the logo search and the segmentation rather than
    running them to conclude nothing — and can override the encoding profile,
    which is the useful half of item 10 without the rule engine.
12. **Run-hours schedule and finish action.** The schedule is ~~done~~: hours
    of the day during which jobs may *start*, with anything already running
    left alone, and the health endpoint reporting the schedule so a queue that
    is not moving explains itself. Sleeping or shutting down when the queue
    empties is still missing. **M**
13. **Output naming from metadata.** ~~Naming and folders are done~~: a
    template over `{title}`, `{channel}`, `{date}`, `{time}`, `{year}`,
    `{month}` and `{source}`, where slashes in the *template* build
    directories and slashes in a *title* cannot. Genre folders still need ARIB
    genre parsing, which `asaborake-ts` does not do.
14. **Source housekeeping.** Duplicate submissions and the free-space check
    are ~~done~~: the same recording to the same output returns the job already
    queued, and a job that could not fit is refused before it starts rather
    than discovered part way through. Moving sources to `succeeded/`/`failed/`
    is still missing. **S**

### Stage 4 — make the UI a console

15. ~~**Submit a job from the browser.**~~ **Done**: pick a recording, a
    profile and a channel from the queue screen.
16. **Queue operations.** Search and status filtering are ~~done~~; reorder,
    priority, duplicate, force-start and delete-with-source are not. **M**
17. ~~**Profile editor.**~~ **Done.** Profiles a deployment adds or changes
    live as TOML files beside the shipped ones and override them by name;
    reverting an override restores what it was overriding. Edited as the TOML
    itself, because a profile *is* a TOML document and a form over it would be
    a second representation to drift from the first.
18. **History browser** with the per-job statistics from stage 1. **M**
19. ~~**Auto-select rule builder.**~~ **Done**, together with the per-channel
    settings panel from stage 3. Both live on one screen, deliberately: a
    channel is the general case and a rule the particular one, and seeing them
    apart would not answer "why did this recording get that profile".

### Stage 5 — go past Amatsukaze

20. **Make the timeline editable.** Drag boundaries, retype a segment, re-cut
    without re-analysing — the analysis is already stored. Amatsukaze has no
    equivalent, and it turns a wrong detection from a lost recording into a
    thirty-second correction. **M**
21. **Show the evidence for a decision.** Clicking a boundary should show why
    it was chosen: the logo score, the silence, the scene change, and how the
    15-second grid voted. **S**

---

## What I would deliberately not do

- **NicoJK comment overlay.** Niconico live comments burned in as subtitles.
  Substantial work, niche audience, and it depends on external services.
- **VFR output and inverse telecine (KFM, QTGMC, D3DVP).** This is most of
  Amatsukaze's filter surface and it rests on the AviSynth plugin ecosystem.
  Reproducing it in Rust is a project of its own, and ffmpeg's `bwdif` is
  adequate for recordings that will be watched rather than archived.
- **Multi-GPU resource scheduling.** One consumer GPU, two workers, is the
  deployment. The complexity buys nothing here.
- **Hash-verified network copies.** Solves a problem — corruption in transit
  from a NAS — that this deployment does not have.
- **AviSynth custom filters.** The extension point is real, but it presumes an
  ecosystem Asaborake is not part of.

Each of these is a deliberate omission, not an oversight. If any becomes
necessary, they are individually tractable.

---

## What I would want decided first

1. **Captions or the logo tool first?** Captions are silent data loss and
   affect every recording. The logo tool is what makes cutting work at all, and
   without it Asaborake is an expensive transcoder. I would do the cheap audio
   fixes (1 and 2) immediately, then the logo tool, then captions — but if
   captions matter to how these recordings are watched, that flips.

2. **Does Asaborake need to erase logos?** Amatsukaze removes the logo from the
   output. Asaborake only detects it. This is a real feature difference and I
   do not know whether it matters to you.

3. **How far should the queue go?** Amatsukaze's queue is an application in its
   own right. Much of it exists because it is a desktop tool driven by hand. If
   EPGStation stays the source of jobs, most of stage 4 is decoration.

4. **Is faithfulness to Amatsukaze's detection a goal?** Running both over the
   same recording and comparing chapter output would tell us whether the
   reimplementation is sound. It would also settle a claim I have been making
   about the algorithms that I have not actually verified.
