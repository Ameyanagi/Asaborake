# Asaborake (朝ぼらけ)

Automatic CM (commercial) detection, cutting and transcoding for Japanese
broadcast MPEG-2 TS — written in Rust, built for a headless Linux box with
NVENC, driven from a web UI, and callable directly as an
[EPGStation](https://github.com/l3tnun/EPGStation) encoder.

---

## Heavily inspired by Amatsukaze

**Asaborake exists because of [Amatsukaze](https://github.com/nekopanda/Amatsukaze)
by [nekopanda](https://github.com/nekopanda), and its companion
[join_logo_scp](https://github.com/nekopanda/join_logo_scp).**

Amatsukaze is the reference implementation of this entire problem domain, and
every core idea in Asaborake comes from it:

- modelling a station logo as a **semi-transparent composite** over unknown
  background, and *learning* that logo from recordings rather than hand-drawing
  a mask;
- deciding commercial boundaries by **combining** logo presence with audio
  silence and scene changes, rather than relying on any one signal;
- exploiting the fact that Japanese CM blocks are laid out on a **15-second
  grid**;
- treating format changes, dropped packets and PTS discontinuities as normal
  rather than exceptional.

Asaborake is a **clean-room reimplementation** — no Amatsukaze code has been
copied. It differs in where it runs and how it is operated:

|                | Amatsukaze                       | Asaborake                          |
| -------------- | -------------------------------- | ---------------------------------- |
| Platform       | Windows                          | Linux (containerised)              |
| Language       | C++ / C#                         | Rust                               |
| Filter/encode  | AviSynth + bundled `.exe` chain  | ffmpeg subprocess, pluggable codec |
| CM detection   | `logoframe` + `join_logo_scp`    | in-process, DP-based segmentation  |
| Logo rectangle | drawn by the operator in a GUI   | found automatically                |
| UI             | WinForms desktop GUI             | web UI (Bun + TanStack + Elysia)   |
| Operation      | desktop app / EDCB server        | long-lived HTTP service            |

See [ATTRIBUTION.md](./ATTRIBUTION.md) for the full write-up, and
[docs/algorithms.md](./docs/algorithms.md), where each algorithm is mapped to
the Amatsukaze component it corresponds to.

The name is the same nod: *Amatsukaze* (天津風) and *Asaborake* (朝ぼらけ) are
both poems from the Hyakunin Isshu.

---

## What it does

Given a recording, Asaborake:

1. reads the transport stream — which PIDs carry the programme, how long it
   really is, whether the picture geometry changes part-way through, whether
   the CAS worked;
2. finds the station logo, learns its opacity and colour, and scores every
   frame against it;
3. detects scene changes and silences;
4. decides which stretches are commercials, and how much it trusts that;
5. transcodes the recording with the commercials removed and chapters written.

If it is not confident, it keeps the whole recording and writes chapters
marking where the commercials are — so you can skip them by hand, and judge
whether it was right. A recording that keeps its commercials is an annoyance; a
recording whose programme was cut away is gone.

## Try it

Asaborake ships no broadcast recordings, so the test material is generated:

```sh
cargo build --release --bin asaborake

# A synthetic programme with two 30-second commercial blocks.
./testdata/generate.sh /tmp/sample.ts 120

./target/release/asaborake probe /tmp/sample.ts
./target/release/asaborake analyse /tmp/sample.ts --channel-id 1024 --logo-dir /tmp/logos
./target/release/asaborake encode /tmp/sample.ts -o /tmp/out.mp4 \
    --profile x264-cpu --channel-id 1024 --logo-dir /tmp/logos
```

`analyse` decodes but writes nothing, and prints where it thinks the
commercials are. That is the fastest way to see whether detection is working on
your own material.

## Commands

```
asaborake probe <in>                  what the recording contains
asaborake analyse <in>                where the commercials are
asaborake encode <in> -o <out>        transcode, cutting them
asaborake epgstation                  run as an EPGStation encoder
asaborake logo list|show|forget       manage learned logos
asaborake profiles                    what this ffmpeg can encode with
asaborake serve                       the job server and its API
```

## Profiles

| Profile      | Encoder      | For                                        |
| ------------ | ------------ | ------------------------------------------ |
| `nvenc-h264` | `h264_nvenc` | the default: 720p, GPU, CPU left for analysis |
| `nvenc-hevc` | `hevc_nvenc` | 1080p archive, smaller, less compatible    |
| `x264-cpu`   | `libx264`    | no GPU — CI, a laptop                      |
| `x265-cpu`   | `libx265`    | no GPU, smallest output, slow              |

`asaborake profiles` marks any whose encoder is missing from your ffmpeg build.

## Running it

With EPGStation: see [docs/epgstation.md](./docs/epgstation.md). There is no
wrapper script — EPGStation's encoder contract is environment variables in and
JSON progress out, and the binary implements it directly.

Standalone, with the web UI:

```sh
docker compose -f docker/compose.yml up -d
```

The engine binds loopback and has no authentication of its own; the web service
is what faces the network.

## Requirements

- **ffmpeg 5.1 or newer** — 5.1 introduced `-fps_mode`, which the frame reader
  needs to keep the frame index and the timestamp in agreement. Asaborake
  invokes ffmpeg as a separate process and never links it.
- **Rust 1.90+** to build.
- **Bun** to build the web UI.
- An NVENC-capable GPU for the default profiles; the CPU profiles need none.

## Developing

```sh
make hooks     # run the same checks as CI before each commit
make check     # fmt, clippy, tests
make reference # clone Amatsukaze and join_logo_scp for reference
```

The suite generates its own test material with ffmpeg rather than shipping
recordings, so it runs anywhere and carries no broadcast content. Tests that
need ffmpeg skip when it is absent.

## Status

Early. The pipeline runs end to end and is verified against generated material:
it finds the logo unaided, detects both commercial blocks to within a tenth of
a second, and produces a correctly cut file with chapters. It has not yet been
run against a large body of real broadcast recordings, which is what the
detection thresholds ultimately have to be tuned against.

Known limits are listed at the end of
[docs/algorithms.md](./docs/algorithms.md).

## Licence

MIT — see [LICENSE](./LICENSE).

Asaborake invokes `ffmpeg` as a **separate process** and does not link against
it, so the licence of your ffmpeg build does not propagate into this project.
