# Attribution

## Amatsukaze

**Asaborake is heavily inspired by [Amatsukaze](https://github.com/nekopanda/Amatsukaze),
written by [nekopanda](https://github.com/nekopanda).**

Amatsukaze is an automated MPEG-2 TS transcoder for Japanese broadcast recordings. It is
the reference implementation of this problem domain: it demuxes a recorded TS, learns the
broadcaster's on-screen logo, detects commercial blocks, cuts them, and re-encodes the
result with correct audio/video sync and chapters. Asaborake sets out to do the same job
on a platform Amatsukaze does not target.

### What Asaborake takes from Amatsukaze

These are Amatsukaze's ideas. Asaborake would not exist without them:

1. **The transparent-logo model.** A station logo is not an opaque stamp; it composites
   over the picture with a per-pixel opacity. Amatsukaze models it as a per-pixel linear
   relationship between the observed frame and the underlying background, and solves for
   the coefficients by least squares. Asaborake uses the same compositing model.
2. **Learning the logo from recordings.** Rather than requiring a hand-drawn mask,
   Amatsukaze scans a recording, finds where a logo is stably present, and derives the
   logo's opacity and colour from the accumulated statistics.
3. **Per-frame logo presence as a signal.** Amatsukaze's `logoframe` scores every frame
   against the learned logo, producing a presence/absence track over the whole recording.
4. **Fusing multiple weak signals.** `join_logo_scp` decides cut points by combining the
   logo track with audio silence and scene changes (produced by `chapter_exe`), because no
   single one of those is reliable on its own.
5. **The 15-second grid.** Japanese CM blocks are laid out in 15-second units, and that
   structural prior is what makes the problem tractable at all.
6. **Treating TS as hostile input.** Format changes mid-recording, dropped packets,
   scrambled sections and PTS discontinuities are normal, not exceptional, and the
   pipeline has to split and resynchronise around them.

### Companion projects

- **[join_logo_scp](https://github.com/nekopanda/join_logo_scp)** — nekopanda's CM cut
  position calculator, the direct ancestor of `asaborake-cmcut`. Its README states that
  redistribution and modification require no prior contact.
- **[chapter_exe](https://github.com/nekopanda/chapter_exe)** — silence and scene-change
  detection, the ancestor of the corresponding detectors in `asaborake-analyze`.
- **[delogo-aviutl](https://github.com/makiuchi-d/delogo-aviutl)** by MakKi — Amatsukaze's
  `LogoScan.hpp` credits this plugin for its `approxim_line()`, `GetAB()` and
  `med_average()` routines. Asaborake does not reuse those routines; its opacity/colour
  estimator is derived independently from the compositing identity (see
  [docs/algorithms.md](./docs/algorithms.md)).

### Relationship and licensing

Asaborake is a **clean-room reimplementation**. No Amatsukaze source has been copied into
this repository. The upstream trees are cloned into `reference/` during development purely
for behavioural reference and are **not redistributed** here — `reference/` is gitignored.
To obtain them:

```sh
git clone --depth 1 --no-recurse-submodules https://github.com/nekopanda/Amatsukaze.git reference/Amatsukaze
git clone --depth 1 https://github.com/nekopanda/join_logo_scp.git reference/join_logo_scp
```

Amatsukaze declares no repository-level licence. Its README states that GPL applies to the
distributed whole because of bundled GPL libraries (libfaad2 and others), while
**nekopanda's own code is offered under the MIT Licence**; the core headers Asaborake
studied (`LogoScan.hpp`, `CMAnalyze.hpp`, `Mpeg2TsParser.hpp`, …) carry MIT headers
individually.

Asaborake bundles no GPL code and links no GPL library. `ffmpeg` is invoked as a separate
process over pipes, so the licence of the operator's ffmpeg build does not propagate into
this project. Asaborake is therefore distributed under the MIT Licence.

## Other projects Asaborake interoperates with

- **[EPGStation](https://github.com/l3tnun/EPGStation)** (MIT) — Asaborake implements
  EPGStation's external-encoder contract. No EPGStation code is included.
- **[mirakc](https://github.com/mirakc/mirakc)** (Apache-2.0/MIT) — the Mirakurun-compatible
  tuner server Asaborake is typically deployed alongside.
