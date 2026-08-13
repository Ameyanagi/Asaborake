# The logo fit returns almost zero opacity for everything

Open bug. The logo tool works — it serves frames, maps a drawn box to source
pixels, runs the scan, reports its measurements — but the fit it produces is
wrong for every rectangle tried, including ones that cannot fail.

## What was measured

All against `/recordings/asaborake-test.ts`, a 20-minute テレビ朝日 recording
with an emergency L字 overlay up throughout.

| Rectangle | Frames | Background spread | Mean opacity | Solid pixels |
| --- | --- | --- | --- | --- |
| `tv asahi` watermark, tight | 984 | 255 of 255 | 0.023 | 0 |
| the same, Amatsukaze's strict border test | 648 | 255 | 0.0005 | 0 |
| the same, with generous margin | 249 | 250 | 0.031 | 0 |
| empty picture, middle of frame | 1848 | 251 | 0.006 | 0 |
| **the L字 banner — opaque white on blue** | 255 | 247 | **0.0004** | 0 |

The last row is the one that settles it. That region is a solid graphic
overlay at full opacity, present for the whole recording. Its opacity should
fit near 1.0 over the text strokes. It fits at 0.0004, indistinguishable from
a box pointed at nothing.

So the material is not the problem and neither is the rectangle. The box was
verified by cropping the source at those exact coordinates with ffmpeg — it is
squarely over the logo.

## What that implies

`alpha = 1 - 1/A`, so a mean opacity of ~0 means the fitted slope `A` is ~1 for
every pixel — the regression is concluding that the observed picture equals the
background everywhere, even where the observed picture is a static opaque
graphic.

The maths was checked against the reference line by line and matches:

```cpp
// Amatsukaze, LogoScan.hpp
approxim_line(n, sumF, sumB, sumF2, sumFB, A1, B1);  // X = foreground, Y = background
approxim_line(n, sumB, sumF, sumB2, sumFB, A2, B2);  // X = background, Y = foreground
A = (A1 + (1 / A2)) / 2;                             // both are dB/dF
```

Asaborake accumulates `add(foreground, background)` in that order, `fit_line`
returns the slope of Y against X as `approxim_line` does, the forward and
reverse fits are combined the same way, and `alpha = 1 - 1/A`. None of that is
wrong.

Which leaves `canonicalise()`, and it is the strongest suspect:

```rust
if !a.is_finite() || !b.is_finite() || *a < 1.0 {
    *a = 1.0;   // "no logo here"
    *b = 0.0;
}
```

Consider a *fully opaque* pixel — the L字 banner above. Its observed value is
constant while the background varies, so `dF/dB` is zero, `1/A2` is infinite,
and `A` is infinite. The correct reading of that is opacity 1.0. What happens
instead is that `is_finite()` fails and the pixel is rewritten as *no logo at
all*, which is the opposite conclusion.

The same trap catches any pixel whose fit lands slightly below one through
noise, which for a strongly opaque logo is most of them: `A` is large and
unstable there, so noise pushes individual pixels either side, and everything
that lands on the wrong side is silently zeroed rather than clamped to opaque.

Amatsukaze does the same test but reacts oppositely: `GetAB` returning false
makes `GetLogo` return `nullptr` for the *whole* logo. It refuses rather than
quietly producing a logo of nothing — which is why the failure is visible there
and invisible here.

## Fixed so far, and what it did not fix

Two real defects were found and fixed, with regression tests:

- **The reverse fit was used without inverting it.** When only one of the two
  regressions succeeded, the code took whichever one it had. The forward fit is
  the slope of background against observed, which is what the model wants; the
  reverse is its reciprocal. For a pixel the logo covers completely it is the
  *forward* fit that degenerates, so every opaque pixel took the reverse branch
  and arrived as a slope near zero — read as "no logo".
- **The degeneracy test was absolute where it had to be relative.** `fit_line`
  rejected a fit when `n·Σx² − (Σx)²` fell below `1e-12`. That difference is
  two large nearly-equal numbers when x barely moves, so for a constant pixel it
  cancelled to floating-point residue *above* the floor and returned a
  meaningless slope instead of refusing.

A synthetic opaque logo now recovers at opacity 0.98 where it previously came
back as nothing, and a nearly-opaque one keeps most of its pixels.

**It changed nothing on real broadcast** — the L字 banner still fits at
0.0003961112815886736, identical to sixteen digits. That is informative rather
than disappointing: on real material every pixel carries compression noise, so
the forward fit never degenerates and neither fixed path is reached. The
failure there is in the *combined* branch, which was not changed:

```rust
(Some((a1, b1)), Some((a2, b2))) if a2.abs() > 1e-9 => {
    (f64::midpoint(a1, 1.0 / a2), f64::midpoint(b1, -b2 / a2))
}
```

For an opaque pixel with noise, the observed value is a constant plus noise
that is uncorrelated with the background. So `a1 = cov(F,B)/var(F)` is a small
number divided by a small number — near zero, with a *sign set by noise* — and
`1/a2` is enormous with the same arbitrary sign. Averaging them gives a large
value of random sign, and every pixel that lands negative is below one and gets
neutralised. That would leave exactly what is observed: no pixel anywhere above
the bar.

Amatsukaze has the same expression, so the difference is upstream of it — most
likely in how much the background actually varies *per pixel* across the
accepted frames, or in the missing chroma planes, which give two more
regressions to disagree with a noisy luma one.

## Where to start

1. Instrument the combined branch for a handful of known-opaque pixels on the
   real recording: print `a1`, `a2`, `var(F)`, `var(B)` and the count of frames
   contributing. The hypothesis above predicts `var(F)` is tiny and the sign of
   `a1` is arbitrary; confirm or kill that before changing anything.
2. If confirmed, the fix is to refuse a pixel whose `var(F)` is negligible
   compared to `var(B)` — that pixel is opaque by definition — and record it as
   opaque rather than letting a noise-driven regression decide.
3. Then port the chroma planes. Amatsukaze fits Y, U *and* V and requires all
   three to succeed, which is three independent votes where Asaborake has one.
   That is the remaining structural divergence and it is likely to matter most
   exactly here, on a grey logo whose luma barely moves.
4. Re-run the five rectangles in the table above. The L字 banner is the
   assertion that matters — it should come back near 1.0.

## The other divergence from the reference

Independent of the above, and worth doing once it is fixed: Amatsukaze fits Y,
U *and* V, tests border flatness on all three planes, and requires the
regression to succeed on each. Asaborake's frame reader hands back gray8, so it
can do neither. That is a structural difference, not a tuning one.
