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

## Where to start

1. Fix `canonicalise` to distinguish the two cases it currently merges: a slope
   at or above one that is merely extreme means *opaque*, and should clamp to
   full opacity; a slope genuinely below one, or a fit that never converged,
   means no logo. Only the second should be neutralised.
2. Then unit-test `LogoScanner` end to end against a synthetic case with a
   *known* alpha, including α near 1.0: composite a known logo over synthetic
   backgrounds spanning the range, accumulate, fit, and assert the recovered
   alpha. The model's algebra has such a test; the scanner's accumulation and
   fit together do not, which is exactly the gap the defect lives in.
3. Re-run the five rectangles in the table above. The L字 banner is the
   assertion that matters — it should come back near 1.0.

## The other divergence from the reference

Independent of the above, and worth doing once it is fixed: Amatsukaze fits Y,
U *and* V, tests border flatness on all three planes, and requires the
regression to succeed on each. Asaborake's frame reader hands back gray8, so it
can do neither. That is a structural difference, not a tuning one.
