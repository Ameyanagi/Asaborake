# Asaborake (朝ぼらけ)

Automatic CM (commercial) detection, cutting and transcoding for Japanese broadcast
MPEG-2 TS — written in Rust, built for headless Linux with NVENC, driven entirely from a
web UI, and callable directly as an [EPGStation](https://github.com/l3tnun/EPGStation)
encoder.

---

## Heavily inspired by Amatsukaze

**Asaborake exists because of [Amatsukaze](https://github.com/nekopanda/Amatsukaze) by
[nekopanda](https://github.com/nekopanda), and its companion
[join_logo_scp](https://github.com/nekopanda/join_logo_scp).**

Amatsukaze is the reference implementation of this entire problem domain, and every core
idea in Asaborake comes from it:

- modelling a station logo as a **semi-transparent composite** over unknown background,
  and *learning* that logo from recordings rather than hand-drawing a mask;
- deciding commercial boundaries by **combining** logo presence intervals with audio
  silence and scene changes, rather than relying on any one signal;
- exploiting the fact that Japanese CM blocks are laid out on a **15-second grid**;
- splitting output on video format changes, and treating audio/video sync and dropped
  packets as first-class concerns.

Asaborake is a **clean-room reimplementation** — no Amatsukaze code has been copied. It
differs in where it runs and how it is operated:

|                | Amatsukaze                       | Asaborake                          |
| -------------- | -------------------------------- | ---------------------------------- |
| Platform       | Windows                          | Linux (containerised)              |
| Language       | C++ / C#                         | Rust                               |
| Filter/encode  | AviSynth + bundled `.exe` chain  | ffmpeg subprocess, pluggable codec |
| CM detection   | `logoframe` + `join_logo_scp`    | in-process, DP-based segmentation  |
| UI             | WinForms desktop GUI             | web UI (Bun + TanStack + Elysia)   |
| Operation      | desktop app / EDCB server        | long-lived HTTP service            |

See [ATTRIBUTION.md](./ATTRIBUTION.md) for the full write-up, and
[docs/algorithms.md](./docs/algorithms.md) where each algorithm is mapped to the
Amatsukaze component it corresponds to (`LogoScan`, `logoframe`, `chapter_exe`,
`join_logo_scp`).

The name is the same nod: *Amatsukaze* (天津風) and *Asaborake* (朝ぼらけ) are both poems
from the Hyakunin Isshu.

---

## Status

Early development. See [docs/](./docs) for design notes.

## Licence

MIT — see [LICENSE](./LICENSE).

Asaborake invokes `ffmpeg` as a **separate process** and does not link against it, so the
licence of your ffmpeg build does not propagate into this project.
