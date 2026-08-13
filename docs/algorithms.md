# How Asaborake decides

Every algorithm here corresponds to a part of Amatsukaze. This document says
which, what the idea is, and where Asaborake does something different and why.
See [ATTRIBUTION.md](../ATTRIBUTION.md) for the credit these ideas are owed.

| Asaborake                        | Amatsukaze                        |
| -------------------------------- | --------------------------------- |
| `asaborake-ts`                   | `Mpeg2TsParser.hpp`, `TsInfo.hpp` |
| `asaborake-analyze::logo::scan`  | `LogoScan.hpp` — `LogoScan`       |
| `asaborake-analyze::logo::locate`| *(none — the operator drew it)*   |
| `asaborake-analyze::logo::detect`| `LogoScan.hpp` — `LogoDataParam`  |
| `asaborake-analyze::scene`       | `chapter_exe`                     |
| audio silence                    | `chapter_exe`                     |
| `asaborake-cmcut`                | `join_logo_scp`                   |

---

## The logo model

A station logo is not an opaque stamp. It composites over the picture with a
per-pixel opacity:

```
observed = (1 - alpha) * background + alpha * colour
```

Asaborake stores the inverse, because removing a logo and scoring a frame
against one both go from observed back to background:

```
background = a * observed + b
```

with `a = 1/(1 - alpha)` and `b = -alpha * colour / (1 - alpha)`. Storing
`(a, b)` keeps the hot loop to one multiply-add per pixel and makes the fit a
plain linear regression. It also means `a` is always at least 1 — a value below
that is not physically possible, and is rewritten as "no logo here" rather than
left to invert the picture.

## Learning it

Fitting that line per pixel needs pairs of (what was observed, what the
background actually was). The background is exactly what a recording does not
tell us — except in frames where the area around the logo happens to be a
single flat colour. A fade to black, a title card, a shot of open sky: in
those, the background under the logo is confidently the same colour as the
border around it.

So the scanner samples the border of the logo rectangle, rejects the frame
unless that border is uniform, and otherwise takes the border's colour as the
background for every pixel inside.

Two things make this work, and both are guarded:

- **A spread of brightnesses.** Every accepted frame having the same background
  — a programme that only ever fades to black — leaves the slope and the
  intercept indistinguishable. The scanner refuses to fit when it sees that.
- **Only frames that carried the logo.** A recording's flat frames include the
  fades inside its *commercials*, where there is no logo. Those say "observed
  equals background", a different relationship, and mixing the two drags the
  estimated opacity toward zero — far enough, on a recording that is a third
  commercials, to be rejected outright.

The second is why learning is three passes rather than one: fit from everything,
use that result to judge which frames actually carried the logo, refit from
those alone. Amatsukaze does the same
(`LogoScan.hpp`: `ロゴのあるフレームだけAddFrame`). The bootstrap fit is held
to a lower bar than the final one, since it only has to be good enough to
recognise its own frames.

## Finding it

Amatsukaze has the operator draw the logo rectangle in its GUI. Asaborake runs
unattended behind EPGStation, so it has to find the rectangle itself. This part
has no upstream counterpart.

A logo is an edge that never moves. Averaging edge strength over time is not
enough — a permanently busy corner averages high too — so each pixel is scored
by the ratio of its mean edge strength to its standard deviation: high and
*steady* rather than merely high.

That is still not enough, because a recording contains long stretches that are
static for reasons of their own: a held title card, a station ident, a test
pattern. Measured across a whole recording, any of those looks steadier than a
logo. What a logo has instead is **persistence** — it is there through the
programme, which is most of the recording, while a static interlude is there
for one stretch and gone. So the recording is scored in chunks and a pixel must
stand out in the majority of them.

Nearby blobs are then clustered, because a Japanese station mark is routinely
several disconnected glyphs and taking only the largest would learn one
character.

## Detecting it

The obvious test — remove the logo and see whether the picture got simpler —
fails, because how much a pixel changes depends on what was behind it. A logo
over black barely moves the numbers; the same logo over white moves them a lot.
Thresholding raw differences tracks the brightness of the programme rather than
the presence of the logo.

Asaborake correlates against the logo's *shape* instead, following Amatsukaze:

1. Composite the logo onto a ladder of uniform grey backgrounds, giving a
   reference at every brightness it might appear over.
2. Pick the pixels whose 5×5 neighbourhood varies most — the logo's edges. Flat
   interior pixels carry no shape information.
3. Keep each such neighbourhood as a zero-mean patch. Zero-mean is what makes
   the match indifferent to background brightness.
4. Precompute, per pixel and per background level, the correlation the
   reference itself produces, and normalise by it, so a perfect match scores 1
   wherever on the ladder it lands.

Each frame is then scored twice: as it is, and with the logo removed. A frame
carrying the logo correlates strongly as-is and near zero once removed. A frame
without it correlates near zero as-is and *negatively* once removed, because
removing an absent logo stamps its photographic negative into the picture.
Combining the two — `max(0, present) + min(0, absent)` — is far sharper than
either alone.

## Scene changes and silence

Scene changes are the mean absolute difference between consecutive 64×36
reductions of the frame. Comparing full frames measures camera shake and film
grain; at that size only a real change of content moves the numbers. A cut has
to clear both an absolute threshold and a multiple of the local median, because
a talking-head scene never reaches an absolute threshold at a real cut and an
action sequence exceeds it constantly.

Silence is 20 ms RMS windows on the audio, downmixed to 8 kHz mono — silence is
broadband, so bandwidth buys nothing. A gap counts when it holds below −50 dBFS
for at least 150 ms, which separates a block boundary from a pause in dialogue.

## Deciding what is a commercial

`join_logo_scp` works as a cascade of rules. That works, but every rule
interacts with every other, so tuning one changes the behaviour of the rest,
and there is no way to ask what the *best* reading of a recording is.

Asaborake states the problem once instead. A recording is a sequence of
segments split at candidate boundaries, each labelled programme or commercial,
and every labelling has a score:

- the logo should be present through programme and absent through commercials;
- Japanese commercial blocks are laid out on a **15-second grid**, so a block of
  30, 60 or 90 seconds is far more plausible than one of 47;
- a boundary where a scene change, a silence and a logo transition all coincide
  is worth much more than a lone scene change, of which a drama has hundreds;
- programmes are long, commercial blocks are not, and neither alternates every
  few seconds.

The best-scoring segmentation is found exactly by dynamic programming, in time
quadratic in the number of candidate boundaries. Each rule above is one term
with one weight, and changing a weight changes only that term.

## Refusing to guess

Every stage reports when the evidence is thin rather than producing an answer
anyway, and the segmenter reports how much it trusts its own result. The
default policy below the threshold is to **keep the whole recording** and write
chapters marking where the commercials are.

That is not merely a safe fallback — it degrades to a manual version of the same
result, since the chapters let a viewer skip exactly what Asaborake would have
removed, and judge whether it was right.

A recording that keeps its commercials is a minor annoyance. A recording whose
programme was cut away is gone.

## Cutting

Cutting by seeking and concatenating means one ffmpeg invocation per kept range
plus a concat pass, and every seek lands on a keyframe rather than the frame
that was asked for. Broadcast GOPs are half a second long, so that is up to half
a second of commercial left in, or half a second of programme taken out, at
every join.

Asaborake selects frames inside a single filter chain instead. The decoder still
decodes everything, which costs time, but every cut lands on the exact frame and
the whole job is one pass.

The timeline is rebuilt from the frame index before selecting, so that time in
the filter means what it meant during analysis: positions are derived by
counting decoded frames, not by reading container timestamps, which broadcast
recordings routinely start at an offset.

## Known limits

- **Occluded watermarks.** A Japanese variety programme carries permanent
  telop banners in the corners, and they sit on top of the station watermark
  for much of its length. A faint translucent mark under a bright caption is
  not recoverable, and if it is covered for more than half the recording it
  does not survive the chunk vote either.

  Observed on a twenty-minute slice of a prime-time variety show: the
  watermark is plainly visible in some frames and completely buried in
  others, and no logo was learned.

  The workaround is the one Amatsukaze users already follow: seed the logo
  store from a programme with a clean corner — a news bulletin — and let the
  variety recordings reuse it. `asaborake analyse --logo-dir …` on such a
  recording is enough to populate it.

- **Programme logos versus station logos.** The persistent corner mark on a
  variety show is often the *programme's* logo rather than the station's. It
  works just as well for detection — it is absent during commercials either
  way — but the logo store is keyed by channel, so a programme logo cached
  under a channel will not be found in the next recording from it. The
  detector then reports no logo and the recording is kept whole, which is safe
  but wasteful.

- **Logo-free detection never cuts on its own.** Timing alone produces
  surprisingly convincing plans — on a real recording it found twelve blocks,
  every one an exact multiple of fifteen seconds — but nothing in timing
  separates a commercial break from a scene change that happens to fall on the
  grid between two silences. Confidence is therefore capped below any sensible
  threshold when no logo was found, so the plan is reported and written as
  chapters but not applied. Setting the low-confidence policy to `cut` applies
  it anyway.

- **HEVC geometry.** Format-change detection parses MPEG-2 sequence headers and
  H.264 SPS. A 4K HEVC service will not have its resolution changes noticed.
- **Timestamp discontinuities.** A recording assembled from separately encoded
  stretches — rather than one continuous capture — cannot be addressed by
  position at all, because ffmpeg restarts both its clock and its frame counter
  at each discontinuity. `asaborake probe` reports them.
- **ARIB captions** are not extracted. A recording's captions are dropped rather
  than carried into the output.
- **Logos in the middle band.** The locator only searches near the top and
  bottom edges. A watermark centred vertically will not be found.
