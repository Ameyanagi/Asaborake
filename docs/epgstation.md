# Running Asaborake from EPGStation

EPGStation runs an external encoder as a plain child process. It hands over
nothing but environment variables, and reads progress back as newline-delimited
JSON on stdout. Asaborake implements that contract directly, so there is no
wrapper script: the binary *is* the encoder.

## Configure it

Add an entry to EPGStation's `config.yml`:

```yaml
encode:
  - name: Asaborake CM Cut
    cmd: '/usr/local/bin/asaborake epgstation --profile nvenc-h264'
    suffix: .mp4
    # How much longer than the recording the job may take before EPGStation
    # gives up. Asaborake decodes the recording once to analyse and once to
    # encode, and learns a channel's logo on the first recording from it, so
    # the first job on a new channel is the slow one.
    rate: 6.0
```

`cmd` is split on spaces, so the binary path must not contain any. EPGStation
checks that the binary exists when it parses the config, and refuses to start
if it does not.

## What Asaborake reads

Everything comes from the environment EPGStation sets:

| Variable                 | Used for                                        |
| ------------------------ | ----------------------------------------------- |
| `INPUT`                  | the recording to transcode                      |
| `OUTPUT`                 | where to write the result                       |
| `CHANNELID`              | the logo store key — **this is the important one** |
| `CHANNELNAME`            | naming a newly learned logo                     |
| `NAME`                   | the title, for logs and the cut record          |
| `RECORDEDID`             | correlating with EPGStation's own logs          |

`CHANNELID` is what makes the second and every later recording from a channel
fast. Asaborake learns that channel's logo once, stores it under that id, and
reuses it — which removes three decoding passes from every subsequent job.

## What Asaborake writes

Progress goes to stdout as EPGStation expects:

```json
{"type":"progress","percent":0.42,"log":"encoding"}
```

`percent` is a fraction; EPGStation's client multiplies by 100.

Logs go to **stderr**, which EPGStation captures into its own debug log. Exit
code 0 means success; on anything else EPGStation deletes the output file,
which is why a failed job never exits zero after writing a partial one.

Beside each output Asaborake writes a `.cut.json` record: what was removed,
why, and how confident it was. That is what makes a surprising result
explainable after the fact without re-running anything.

## Where the logos live

Point Asaborake at a directory it can write to, and mount it somewhere that
survives a container restart:

```yaml
encode:
  - name: Asaborake CM Cut
    cmd: '/usr/local/bin/asaborake epgstation --profile nvenc-h264 --logo-dir /var/lib/asaborake/logos'
```

Or set `ASABORAKE_LOGO_DIR` in the environment.

Without it, Asaborake still works — it simply relearns the logo from scratch on
every recording, which costs three extra decoding passes each time.

## Running it in the same container as EPGStation

Asaborake is a single static-ish binary with ffmpeg as its only dependency, and
the EPGStation image already has ffmpeg. Copying the binary in is enough:

```dockerfile
COPY --from=asaborake/engine:0.1.0 /usr/local/bin/asaborake /usr/local/bin/asaborake
```

This is the simplest deployment, and the one to start with. It runs each job in
EPGStation's own encode slot, so `concurrentEncodeNum` governs how many run at
once.

## Running it as a service instead

The alternative is to run `asaborake serve` and have the EPGStation encoder
submit to it. That gives one queue for everything — jobs from EPGStation and
jobs submitted by hand appear together, with a web UI over both — and lets
Asaborake manage its own concurrency rather than borrowing EPGStation's.

```yaml
encode:
  - name: Asaborake CM Cut
    cmd: '/usr/local/bin/asaborake epgstation --profile nvenc-h264'
    suffix: .mp4
    rate: 6.0
```

with `ASABORAKE_SERVER=http://asaborake-engine:8081` in EPGStation's
environment. The encoder becomes a thin client: it submits the job, streams the
progress back out in EPGStation's format, and exits with its result.

Both containers must see the recordings at the **same path**, because a job
carries absolute paths.

## Checking it works

Before wiring it into EPGStation, run it by hand against a recording:

```sh
asaborake probe /recordings/something.ts
asaborake analyse /recordings/something.ts --channel-id 3239123 --logo-dir /var/lib/asaborake/logos
```

`analyse` decodes but writes nothing, and prints where it thinks the
commercials are. If that looks right, the encode will be right.
