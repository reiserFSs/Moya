# Photoshop pixel-operation mathematics for a WGSL reimplementation

## Evidence boundary

This reference combines public Photoshop documentation with our own operation-level pixel measurements. Adobe documents many operation semantics but does **not** publish exact Gaussian-radius calibration, blur kernels, radial sample counts, fixed-point rounding, or all HDR clamp rules. Unresolved behavior is marked **UNCERTAIN** or explicitly described as uncalibrated. Candidate kernels and transfers are implementable approximations unless a scoped measurement establishes otherwise; they are not claims of universal Photoshop parity.

The calibration sections below retain historical measurements. Their command
and temporary-output examples use the current Moya naming; the rename itself
does not imply those commands were rerun or historical artifacts were moved.

## 1. Shared numeric and color conventions

Use component-wise floating-point RGB unless an operation explicitly couples channels. Define `sat(x)=clamp(x,0,1)` and `mix(a,b,t)=a*(1-t)+b*t`.

| Document | Logical channel range | Baseline storage emulation |
|---|---|---|
| 8-bpc | integer 0…255 | `Q8(x)=floor(255*sat(x)+0.5)/255` |
| Photoshop 16-bpc | integer **0…32768 inclusive**, 32769 values | `Q16(x)=floor(32768*sat(x)+0.5)/32768` |
| 32-bpc HDR | IEEE floating-point; 1 is reference white, not maximum radiance | retain f32, including values above 1; operation-specific handling of negatives |

“Photoshop 16-bit is 15-bit” is useful shorthand but not quite exact: it is **15-bit + one endpoint**, not 0…32767. A PNG/TIFF file's 0…65535 range is not Photoshop's internal channel coordinate range. Adobe-community evidence explicitly confirms 32769 values and cites Adobe engineer Chris Cox. [Depth evidence](https://community.adobe.com/questions-712/question-about-16bit-images-in-photoshop-1069902). **UNCERTAIN:** round-to-nearest above is a deterministic implementation baseline, not a verified tie-breaking/truncation rule for every Photoshop command. Likewise, do not assume every internal subpass is quantized; quantize at externally visible destructive operation boundaries for an initial emulation.

Normally 8/16-bit operations act directly on the document's encoded RGB numbers; 32-bit HDR stores light-proportional values. Do not silently use an sRGB texture view: wgpu `_srgb` sampling decodes RGB, which would change encoded-space blurs and arithmetic. Use non-sRGB float/UNORM resources and explicit conversion. [Adobe HDR](https://helpx.adobe.com/photoshop/using/high-dynamic-range-images.html).

This is **not** an unconditional “8/16 gamma, 32 linear” rule: the document can have a linear ICC profile, and Photoshop's **Blend RGB Colors Using Gamma** preference changes compositing. With the preference disabled Adobe says it blends directly in document color space; gamma 1.00 selects colorimetrically correct blending. Its effect on every exotic blend-mode substep is **UNCERTAIN**. [Adobe color settings](https://helpx.adobe.com/photoshop/using/color-settings.html).

For an explicitly sRGB-based implementation:

- Decode: `D(c)=c/12.92` if `c<=0.04045`, otherwise `((c+0.055)/1.055)^2.4`.
- Encode: `E(l)=12.92*l` if `l<=0.0031308`, otherwise `1.055*l^(1/2.4)-0.055`.
- For an actual ICC profile use that profile's transfer curves, not a universal exponent 2.2. Some published Photoshop reverse-engineering approximates sRGB with 2.2; this is not exact sRGB.

## 2. Gaussian Blur (`gaussianBlur`)

**Documented:** Gaussian-weighted neighborhood average, radius in pixels; supports 8/16/32-bit. Selected pixels may read source pixels **outside the selection**. Thus blur the source then mix the result through the selection; do not treat the selection border as the image border. [Adobe filter reference](https://helpx.adobe.com/photoshop/using/filter-effects-reference.html).

**Measured PS 27.11 Float kernel:** it is a finite box spline, not a sampled exponential Gaussian. For radii 3, 4, 5, 10, 25, 50, 100, 250 and 1000, the following width generator reproduces impulse coefficients to <3e-9:

1. Choose odd `n=floor_odd(sqrt(12*r*r/5+1))`.
2. Set `m=ceil((12*r*r-5*n*n-20*n-15)/(-4*n-4))`; convolve `m` boxes of width `n` and `5-m` of width `n+2`.
3. From residual variance `v=r*r-sum((w*w-1)/12)`, greedily append odd box widths `floor_odd(sqrt(12*v+1))` while at least 3, subtracting each variance.

Examples: r5 gives `[5,7,7,7,7,9]`, r10 `[3,3,15,15,15,15,17]`, r25 `[7,37,39,39,39,39]`. Support is half the sum of `(w-1)`, not `ceil(4*r)`. Variance approximates r²; chained splines are not exactly a single blur at the variance-added radius.

**Small-radius regime:** captured r0.1, .25, .5, .75, 1, 1.5 and 2 kernels use exact 1/256 weights. Half-kernels include r.5 `[144,55,1]/256`, r1 `[96,60,18,2]/256`, r2 `[48,44,31,17,8,3,1]/256`. At r2.5, the half-kernel is `[301,282,232,166,102,52,21,6,1]/2025`. The implementation interpolates these measured radius anchors; unmeasured intermediate radii remain uncalibrated.

Convolve the complete kernel separably with replicated canvas edges. The corpus edge/diagonal/natural r5 probes agree to one integer code or Float roundoff. Transparent inputs and selection-relative edge semantics are not measured.

**Large radii:** the GPU retains endpoint-aligned downsampling for bounded per-pixel cost, resampling the actual full-radius spline weights onto that grid. This is an approximation, not an exact cascade; large-radius impulses still carry downsampling residual. The CPU reference uses the full spline.

**Depth:** encoded RGB for normal 8/16-bit, native values for Float. The filter API has no depth parameter; corpus comparison quantizes the completed filter. Integer intermediate rounding remains unresolved (observed residual up to five 16-bit codes).

## 3. Motion Blur (`motionBlur`)

**Documented:** angle in degrees, blur distance/intensity 1…999 pixels, simulating constant-exposure movement; 8/16/32 supported. [Adobe filter reference](https://helpx.adobe.com/photoshop/using/filter-effects-reference.html).

**Measured PS 27.11 raster kernel:** positive angles follow `(cos(theta),-sin(theta))`. Project distance onto the axes with `trunc(D*cos(theta)+.5)` / `trunc(D*sin(theta)+.5)` and use the larger absolute projection as the span. Both endpoints are included: an axial distance 1 is two taps, not identity. Odd spans introduce a half-pixel centroid shift.

**Minimum descriptor distance:** `reference-corpus/motion-zero` captures distance0/1/2 at −45° in a native16 document. All three Photoshop outputs are pixel-identical and differ from the input by up to 9896 TIFF codes. Zero therefore selects the minimum kernel, not identity; the GPU clamps nonnegative distances below one to one. Negative distances are not established by these captures.

Angled Float kernels are consistent with two 1/128-pixel quantized shear interpolations around the major-axis box, rather than a single bilinear line sample or tapered box. For 45° the transverse weights are `[.25,.5,.25]`. Canvas coordinates are clamped.

**Measured integer-depth rule:** the two shears are not the Float linear reconstruction with extra rounding. They use the cubic convolution kernel with parameter `a=-.75`, clamp to [0,1] and round to the document grid after the first shear, then round each major-axis box average before the inverse cubic shear. Clamp the inverse-shear result. At 45°/distance1 the native16 impulse coefficients are `.1484375/.3525390625/.1484375` at each of two major coordinates, versus Float `.125/.25/.125`; rounding Float coefficients cannot explain that difference. The cubic positive half-phase coefficient `.59375`, clipped negative lobes and the second shear explain the observed gain. Eight-bit short-distance natural captures discriminate rounding after both stages: focused distance2 aggregate RMSE falls from .636 (neither) through .582 (first only) to .461 (both).

The GPU API now takes document depth. Short spans use a four-tap first shear with two guard pixels, preserving source-edge extension before the inverse shear. Long spans use a shear-space inclusive prefix sum, then endpoint-extended box differences and the inverse shear; work is logarithmic in canvas size rather than proportional to distance. This removes the old hierarchical quadrature, which missed natural distance500/2000 probes by as much as .35046 Float.

**Measured model errors (CPU double-precision model, not a post-edit GPU verification):**

| Corpus | Depth | Before max | New model max / RMSE |
|---|---|---:|---:|
| Original 35 cases/depth | 8 | 26 | 2 / .039523 |
| Original 35 cases/depth | 16 | 3360 | 142.807 / .886987 |
| Original 35 cases/depth | Float | .003692 | .00369151 / .000026526 |
| Focused natural 12 cases/depth | 8 | — | 5 / .342562 |
| Focused natural 12 cases/depth | 16 | — | 671.800 / 1.816591 |
| Focused natural 12 cases/depth | Float | — | .00369151 / .000039754 |

The original corpus satisfies every motion target under this model. `reference-corpus/motion-focus` records 36 successful real PS captures at angles30/45/−60, distances2/20/500/2000 and all three depths. Residual: distance2000/−60 at the top-right canvas corner differs by 5 byte codes / 671.800 native16 codes (two native16 channel samples exceed the 385.5-code target); Float passes. This extreme corner case is **not established parity**. Nonintegral shear phase remains approximate (Float max .00369151); do not describe the entire measured argument range as exact.

## 4. Radial Blur, Zoom (`radialBlur`, zoom method)

**Documented:** zoom blur follows radial lines from a chosen center, amount 1…100. Draft is fast/grainy; Good and Best are smoother, often visibly indistinguishable except on a large selection. Adobe supplies **no numeric sampling table**. Supports 8/16/32-bit. [Adobe filter reference](https://helpx.adobe.com/photoshop/using/filter-effects-reference.html).

**Measured descriptor center (PS 27.11):** raw `center.horizontal/vertical` are canvas-normalized and clamped to [0,1]; `centerSpace` is harness metadata, not a Photoshop argument. The measured pixel-coordinate center is `c = floor(clamp(center,0,1)*128) * size/128 - 1` componentwise. This replaces `center*size-.5`. On the 256px impulse, horizontal descriptors .490/.495/.499/.503/.507/.511/.515/.523 produce center bins 123/125/125/127/127/129/129/131. The odd 65×49 stationary impulse at (31,23), descriptor (.5,.5), amount100 Best gives peak .88013136, consistent with the half-pixel center (31.5,23.5), not (31,23). Evidence: `radialBlur-center-grid-*`, `radialBlur-odd-*`, `radialBlur-station-odd` in `reference-corpus/ops/manifest.json`. The GPU API accepts normalized coordinates; `scale_about` still accepts pixels.

**Measured support/model:** samples span inward and outward scales, `q=c+(p-c)*(1+(A/100)*(t-.5)*.5)` for `t` in [0,1], i.e. ±A/400 of radial distance, with bilinear reconstruction. Use the uniform sampling expectation. Draft/Good/Best retain 16/64/256 deterministic midpoint taps: these are integration budgets, **not measured Photoshop tap counts**. Pixel-dependent sparse sample phases remain unmatched; substituting an unrelated random seed would not establish parity.

**Operation-range evidence:** all 96 added PS captures succeeded without preference changes; documents were closed. `radialBlur-focus-{8|16|32}-{draft|good|best}-{center|fraction|repeat|ca}` covers amount10 and amount100; `fraction` uses (.503,.497). Additional exact repeat probes use IDs `radialBlur-residual-{depth}-{tag}-{0..3}`. The following are **CPU sampling-model measurements**, not a post-edit GPU run; 16-bit errors are Photoshop's 0…32768 units (`TIFF error *32768/65535`).

| Coverage | 8-bit max / RMSE | 16-bit max / RMSE | Float max / RMSE | Blur targets |
|---|---:|---:|---:|---|
| Amount10 Good centered/fractional, original plus focus | 2 / .008735 | 190.503 / 1.00117 | .00590498 / .000026542 | All depths pass |
| Amount10 Good offcenter impulse (.25,.75), original `*-offcenter-*` | 10 / .048318 | 1168.518 / 6.28067 | .05532535 / .000272867 | Max fails; RMSE passes |
| Amount100 Draft, centered focus | 10 / .050177 | 1369.021 / 5.36234 | .03224850 / .00013092 | Max fails; RMSE passes |
| Amount100 Best, centered focus | 9 / .050329 | 1254.519 / 6.31800 | .02413619 / .00010983 | Max fails; RMSE passes |

The 195-case pre-repeat impulse set (original147 + focus36 + center-grid8 + odd3 + stationary1) has measured model worst max 62 codes / 7303.111 PS16 units / .23383737 Float; pooled per-image RMSE .133080 / 18.22067 / .000500844. This is a bounded approximation, not all-depth radial parity.

**Irreducible exact-pixel residual:** four identical-operation captures at each depth demonstrate that the failed maximum thresholds cannot be satisfied simultaneously by any deterministic output. At a fixed pixel, any fixed prediction has worst error at least half the observed PS repeat range (triangle inequality). The table gives the observed range, then that lower bound; every bound exceeds the configured max target 3 / 385.505882 / .011764706. Quality-specific deterministic phases cannot remove this repeat discrepancy.

| Repeat tag / operation | 8-bit range → bound | PS16 range → bound | Float range → bound |
|---|---:|---:|---:|
| `rays-offcenter`: amount10 Good, (.25,.75) | 22 → 11 | 2379.036 → 1189.518 | .08117913 → .04058956 |
| `worst`: amount50 Draft, (1,1) | 63 → 31.5 | 8156.124 → 4078.062 | .22340393 → .11170197 |
| `ca-draft`: amount100 Draft, (.5,.5) | 13 → 6.5 | 2703.041 → 1351.521 | .08444512 → .04222256 |
| `ca-best`: amount100 Best, (.5,.5) | 14 → 7 | 2296.035 → 1148.018 | .06446242 → .03223121 |

Float witness pixels (x,y), red channel: rays-offcenter (130,126) has `[.03390495,.04810543,.08117913,0]`; worst (142,142) has `[0,0,.22340393,0]`; ca-draft (128,129) has `[.08048952,.05766654,.10708129,.14211166]`; ca-best (129,128) has `[.13422179,.06975937,.08342648,.07302475]`. The original .233932 residual is therefore not explained by the small centered amount10 repeat variance alone: the offending settings themselves have been repeated. No exemption or tolerance was changed.

Regression `moya-gpu/tests/radial_calibration.rs` checks the center quantization/clamping and symmetric support on odd dimensions, amount0/10/50/100, every quality, plus captured centered amount10 anchors from all three depths. It does not assert equality to Photoshop's nondeterministic neighboring impulse pixels.

## 5. Levels (`levels`)

Let normalized input endpoints be `b,w`, output endpoints `ob,ow`, and midpoint gamma `g>0`. For 8/16-bit, when descriptor endpoint units are 0…255 use `b=black/255`, `w=white/255`, etc., **even though 16-bit pixels use 0…32768 internally**. Do not divide 0…255 descriptor fields by 32768.

**Implementable standard transfer:**

`t=sat((x-b)/(w-b))`;
`y=ob+(ow-ob)*pow(t,1/g)`.

Thus gamma >1 brightens midtones. Output endpoints can compress or reverse the range. Per-channel Levels apply to the targeted channel; a master RGB adjustment applies the same scalar transfer to each component, not to a computed luminance. Preserve alpha unless alpha/mask is the explicit target. Require `w>b` for this formula; equal/reversed input endpoint behavior is **UNCERTAIN**, not something to repair by silently inserting an arbitrary epsilon.

**Measured integer update (Photoshop 27.11):** clamp the gamma control to 0.01…9.99. For gamma ≤1 the power rule applies. For gamma >1, set `q=round(4096*2^(-g-1/(g-1)))/4096` and `s=2^g`. Below `q`, replace power by `A*t³+B*t²+s*t`, with `A=(q^(1/g)/(g*q)+s-2*q^(1/g)/q)/q²` and `B=(3*q^(1/g)/q-q^(1/g)/(g*q)-2*s)/q`. This joins power continuously in value and first derivative. If q rounds to zero, retain power. Eight-bit input normalization rounds `t` to 255 steps **before** gamma; 16-bit source pixels first snap to the native 32768 grid. The focused gamma sweep (1.2/1.5/1.7/2/2.2/3/4/5/10) has max one 16-bit code in every case; gamma 4 discriminates the 4096 junction grid from 2048. The full 27-case integer sweep passes: max 1 at 8-bit and 16 at 16-bit; remaining nonidentity-endpoint residual is LUT normalization precision, not an unresolved gamma direction.

**32-bit measured update (Photoshop 27.11 corpus):** `t=(x-b)/(w-b)` is **not clipped at either endpoint**; `y=ob+(ow-ob)*sign(t)*abs(t)^(1/g)`. Descriptor endpoint units stay 0…255. Across 27 captured ramp256/ramp4096 cases (gamma 0.45/1/2.2, black 0/16, white 235/255, output 0…255 or 16…235), the GPU max error is 0.000002146, RMSE 0.0000003841, PSNR 128.311 dB. Endpoint clamping previously caused max 0.304431. Values above white extrapolate and values below black remain negative, even at noninteger gamma.

## 6. Exposure (`exposure`): exposure, offset, gamma

Adobe explicitly states Exposure performs calculations in **linear color space, gamma 1.0**, including when used on 8/16-bit documents. [Adobe Exposure](https://helpx.adobe.com/photoshop/desktop/adjust-color/hdr-and-channel-manipulation/adjust-hdr-exposure-and-toning.html).

Let linear input be `L`, exposure stops `e`, offset `o`, and gamma correction `g>0`.

Single-control equations, supported by published Photoshop experiments:

- Exposure only: `L'=L*exp2(e)`.
- Offset only: `L'=L+o`.
- Gamma only for nonnegative L: `L'=pow(L,1/g)`.

[Measured analysis and equations](https://geraldbakker.nl/psnumbers/exposure.html); [32-bit formula image](https://impro.usercontent.one/appid/oneComWsb/domain/geraldbakker.nl/media/geraldbakker.nl/onewebmedia/Numbers-exposure-4/EC4-Formulas32.png).

**Measured PS 27.11.0 combined operation:** `T=L*exp2(e)+o`, then `L'=sign(T)*pow(abs(T),1/g)`. Negative Float values survive gamma correction, and values above one remain unclipped. Focused captures at `reference-corpus/exposure-focus` include positive/negative offsets, simultaneous exposure/gamma, and HDR input; they distinguish offset-after-exposure and gamma-after-offset from the alternatives.

**Measured 8/16-bit path on this corpus:** decode with `L=max(x,0)^(563/256)`, apply the linear operation, clamp to [0,1], then encode through a 4096-step linearly interpolated inverse-power lookup. Specifically `t=4096*clamp(L',0,1)`, `j=floor(t)`, and output is the interpolation between `(j/4096)^(256/563)` and `(min(j+1,4096)/4096)^(256/563)`. Exact identity bypasses the lookup. This is neither exact sRGB nor an analytic gamma-2.2 round trip; the lookup creates a measurable linear toe in shadows. Quantization remains at the destructive document boundary.

The Float oracle matches float32 reciprocal-gamma power. Native GPU `pow` differed by ~0.043 near output 48000; range-reduced hi/lo lookup interpolation reduces observed max error to 0.007812. All captured Float samples satisfy the approved `max(0.001,4 ULP(abs(PS)))` per-sample tolerance. The lookup uses a gamma-adaptive mantissa grid plus binary-exponent factors, with compensated interpolation/multiplication and no image readback.

## 7. Hue/Saturation (`hueSaturation`) and Desaturate

**Measured Photoshop 27.11:** master adjustment order is **lightness RGB LUT → hue → saturation**, not a final lightness mix. The operation is neither HSV saturation nor a luminance-weighted grayscale conversion, and is distinct from Hue/Saturation blend modes. Measurements use the existing HSV corpus at 8/16/32 bits, plus fourteen focused byte-hue captures in `reference-corpus/hue-focus`.

### Master lightness, Colorize off

Let `l=lightnessSlider/100` and `C` be each input RGB channel. Float uses `C₁=C*(1-l)+l` for `l>=0`, or `C₁=C*(1+l)` for `l<0`, without clipping.

Integer depths first snap input to their storage grid. For byte input `x` in 0…255, the lightness LUT truncates the **distance**, not the final result:

- `l>=0`: `x₁=x+floor((255-x)*l)`.
- `l<0`: `x₁=x-floor(x*(-l))`.

At 16 bits, evaluate the Float lightness expression on the 32768 grid and round `floor(C₁*32768+.5)/32768`. Recover a normalized TIFF input's word `w=floor(C*65535+.5)` before snapping: its internal value is `floor((w+1)/2)/32768`. Direct Float multiplication by 32768 can round large even TIFF words onto false half-ties, which saturation +100 amplifies.

### Hue coordinates

For lightness-adjusted RGB, let `M=max(r,g,b)`, `m=min(r,g,b)`, `d=M-m`, `L=(M+m)/2`, and `S=d/(1-abs(2*L-1))` when `d>0` (zero otherwise). At 16 bits and Float, conventional HSL hue in turns is:

- R maximum: `H=fract(((g-b)/d)/6)`.
- G maximum: `H=((b-r)/d+2)/6`.
- B maximum: `H=((r-g)/d+4)/6`.

Rotate by `H'=fract(H+hueDegrees/360)` and reconstruct RGB with the usual HSL chroma/triangle permutation, preserving `L` and `S`.

**Byte hue has different integer coordinates.** For `d=0`, set `h=0` (hue has no effect on gray). Otherwise compute the signed quotient from byte RGB, independently rounding each division with `floor(value+.5)`:

- R maximum: `h=floor((g-b)*255/d+.5)`.
- G maximum: `h=floor((b-r)*255/d+.5)+510`.
- B maximum: `h=floor((r-g)*255/d+.5)+1020`.

Add the separately rounded slider `floor(hueDegrees*4.25+.5)`, then wrap modulo **1530**. For sextant `k=floor(h/255)` and remainder `f=h-255*k`, the middle-channel fraction is `f/256` in even sextants and `(255-f)/256` in odd sextants. Reconstruct the corresponding max/middle/min RGB permutation. The **255-unit hue sextants versus 256-unit middle reconstruction**, and separate slider rounding, explain the former two-code byte residual; no negative-hue special case is used.

### Saturation and depth

After hue reconstruction, recompute `L=(max(C₂)+min(C₂))/2`. With `s=saturationSlider/100`, scale RGB chroma around `L`:

- `s<=0`: `a=1+s`.
- `s>0`: `a=1/max(1-s,256/32767)`.
- Integer depths only, for nonzero chroma: further limit `a` to `(1-abs(2*L-1))/(max(C₂)-min(C₂))`.
- Output: `Cout=L+(C₂-L)*a`.

Thus Float saturation +100 uses the finite reciprocal **32767/256**, not full HSL saturation and not clipping to [0,1]. No transfer-function conversion is applied inside this adjustment. Full desaturation, including the separate `desaturate` event, yields `(max+min)/2`, not `0.299R+0.587G+0.114B`.

For byte **Hue/Saturation −100**, the final grayscale code is
`floor((max(R,G,B)+min(R,G,B))/2)` after the lightness LUT, not a
half-up midpoint reconstructed through floating HSL. The existing HSV
captures at hue −90/0/90, saturation −100 and lightness 0 match exactly.
The separate byte `desaturate` event independently matches the same floor
rule on the existing HSV capture (zero mismatches; half-up differs at 48,896
pixels). Both byte paths now truncate; 16-bit and Float are unchanged.

### Colorize and measured limits

Colorize instead takes input HSL lightness, sets absolute `H=hueDegrees/360` and `S=clamp(saturationSlider/100,0,1)`, reconstructs RGB, then applies the lightness mix. The captured Colorize matrix covers hue 0/120/240, saturation 20/100 and lightness 0 at all three depths; lightness-nonzero Colorize and input HDR excursions outside [0,1] are not established by these captures. Range-specific color selections are unsupported.

Scoped comparisons of 35 cases/depth, including Colorize, now pass: byte max **1/255** (previously 127/255), 16-bit max **4/32768** (previously 15934/32768), Float max **0.000114** (previously 62.501999, PSNR −17.910 dB). Fourteen additional real byte captures at hue ±1/7/30/45/90/120/179 and saturation +100 all pass max 1/255. Desaturate max errors are 1/255, 1/32768 and exactly zero at 8/16/32 bits.

## 8. Apply Image / Calculations and separable blend modes

Use `B` for destination/base and `S` for source/blend, normalized to the appropriate working numerical domain. Invert source channels first when requested: `S→1-S` (HDR out-of-range inversion behavior **UNCERTAIN**). Apply Image targets a layer/channel; Calculations combines individual channels and may produce a new channel, selection, or document. Both combine corresponding pixel positions; dimensions must match. [Adobe channel calculations](https://helpx.adobe.com/photoshop/using/channel-calculations.html).

**Measured Apply Image corpus (Photoshop 27.11.0):** 108 cases cover Normal, Lighten, Multiply, `divide`, Overlay and Soft Light, opacity 20/60/100%, invert off/on, and depths 8/16/32. The runner constructs `calculationType` from the case name and removes the temporary source layer afterward; this is not layer compositing. `calculationType:divide` produces **Normal**, not arithmetic division: at 100% its output equals the source (inverted when requested), within 0 / 1 TIFF code / 5.96e-8 at 8/16/32 respectively. The comparator therefore interprets the captured `divide` descriptor as Normal, without changing the runtime `BlendMode::Divide` formula or layer-mode parsing.

Narrow release-binary comparisons before / after this descriptor interpretation:

| Depth (metric units) | Compared / rejected | Before max / RMSE | After max / RMSE |
|---|---|---|---|
| 8 (255) | 36 / 0 | 255 / 40.823899 | 1.000015 / 0.171314 |
| 16 (32768) | 36 / 0 | 32768 / 5248.239082 | 2.108887 / 0.468762 |
| Float (1) | 24 / 12 | 5808.598494 / 8.219997 | 5.960464e-8 / 1.5237e-8 |

The after measurement used the existing release kernel with only `divide` descriptors rewritten to Normal in a temporary manifest; it isolates the dispatch fix, not a verification of a rebuilt executable. The 8-bit 1.000015 residual represents one physical code plus f32 normalization error; the old strict reporter marks that group FAIL rather than rounding it away. Normal/Lighten and both inversion settings already matched before this fix; no global source/destination or opacity swap was warranted. Multiply and Overlay retain one-code integer rounding differences; Soft Light's native implementation also differs slightly from the W3C candidate (16-bit maximum above is Soft Light). Native 32-bit Overlay/Soft Light were rejected and are not parity evidence.

`scale:1, offset:0` in these manifest cases are **not forwarded by runner.jsx** to the generated calculation descriptor. This corpus does not measure Add/Subtract or nondefault Scale/Offset, HDR out-of-range inversion, or partial source/destination alpha. Those remain uncalibrated rather than receiving invented adjustments. Existing GPU tests cover source-alpha independence and destination-alpha preservation; the calculation-enum regression covers bare and descriptor-valued names separately from layer blends.

For LDR values in `[0,1]`, concrete scalar blend functions:

| Mode | `F(B,S)` |
|---|---|
| Normal | `S` |
| Multiply | `B*S` |
| Screen | `1-(1-B)*(1-S)` |
| Linear Dodge (Add) | `min(1,B+S)` |
| Linear Burn | `max(0,B+S-1)` |
| Darken / Lighten | `min(B,S)` / `max(B,S)` |
| Difference | `abs(B-S)` |
| Exclusion | `B+S-2*B*S` |
| Subtract (layer mode) | `max(0,B-S)` |
| Divide | `min(1,B/S)`, with zero-source convention requiring measurement at `B=S=0` |
| Overlay | if `B<=0.5`: `2*B*S`; else `1-2*(1-B)*(1-S)` |
| Hard Light | Overlay with B/S exchanged |
| Color Dodge | if B=0 then 0; else if S=1 then 1; else `min(1,B/(1-S))` |
| Color Burn | if B=1 then 1; else if S=0 then 0; else `1-min(1,(1-B)/S)` |

Photoshop's descriptions support the principal semantics: [Adobe blend modes](https://helpx.adobe.com/photoshop/desktop/repair-retouch/adjust-light-tone/blending-mode-descriptions.html). The explicit standard mathematical versions are in [W3C compositing and blending](https://www.w3.org/TR/compositing-1/#blending). **UNCERTAIN:** W3C is not a guarantee of PS integer rounding or every degenerate endpoint. Linear Dodge's clipping/opacity interaction and Photoshop Fill opacity special cases should not be inferred from this table; the table defines full-strength LDR results followed by ordinary layer-opacity mixing.

**Apply Image/Calculations Add and Subtract:** these are distinct from ordinary layer modes because of Scale/Offset:

- Add: `T=(B+S)/scale + offset/255`.
- Subtract: `T=(B-S)/scale + offset/255`.
- LDR result: `F=sat(T)`.

**Order matters: divide first, add offset second, clamp afterward.** Do not clamp `B+S` or `B-S` before scaling/offset. Adobe documents Scale 1.000…2.000 and Offset -255…255, as well as target-minus-source subtraction. Offset above is in 8-bit-style interface units; direct translation to a specific 16/32 descriptor's representation is **UNCERTAIN** without the descriptor/API evidence. In particular, a “128 offset” may have fixed-point endpoint details that differ from exact `128/255`; do not assert equivalence to exactly 0.5 in all PS paths.

At operation opacity `p=opacityPercent/100` and scalar operation mask `m`, for an opaque channel baseline:

`O=mix(B,F,p*m)`.

“Invert mask” means `m→1-m`. Preserve Transparency concerns destination coverage/alpha, not simply multiplying RGB by alpha. Its exact interaction with partially transparent source layers is **UNCERTAIN**; opaque channel math above is not the whole layer-alpha implementation.

**32-bit warnings:** Adobe's HDR-supported layer-mode list includes Normal, Multiply, Darken, Lighten, Linear Dodge, Difference, Subtract, Divide, and nonseparable color modes; **Screen is absent**. Do not silently claim Screen is a native 32-bit PS layer operation. [Adobe HDR support list](https://helpx.adobe.com/photoshop/using/high-dynamic-range-images.html).

For HDR-supported modes, native linear candidate formulas are Multiply `B*S`, Linear Dodge `B+S`, Difference `abs(B-S)`, Darken/Lighten min/max; do not impose a generic upper clamp. Subtract's negative-result policy, Divide singularities, and Apply Image Add/Scale/Offset HDR behavior are **UNCERTAIN**. Algebraically extending Screen above 1 can yield negative/strange values and is not proof of Photoshop semantics.

**Measured Color mode (operation corpus):** Float uses `S + lum(B) - lum(S)`, with `lum(C)=.30R+.59G+.11B`, and **no gamut clipping**. Integer Color instead uses weights `(9830,19334,3604)/32768`; round both luminances to the document grid (255 or 32768) before subtracting. Shift S by that difference. ClipColor works in integer codes about rounded base luminance `L`: for the low branch, `d=L-min(C)` and `C=L+trunc(((C-L)*L+floor(d/2))/d)`; for the high branch, `d=max(C)-L` and `C=L+trunc(((C-L)*(N-L)+floor(d/2))/d)`, where `N` is 255 or 32768. Signed division truncates toward zero, and the half-denominator bias is itself integer. Python TIFF analysis matches every channel exactly for the 8/16-bit RGB-cube full Color cases (`00241`, `00528`) and both 8-bit Color copy stacks (`00922`, `00923`). The gray Color-layer merge used for luminance extraction is exactly the rounded base luminance; its 8-bit oracle (`00284`) also matches all 65,536 pixels. Float RGB-cube residual is 2.46e-7, and the HDR pair's residual is 2.35e-6.

## 9. Layer opacity and alpha compositing

For opaque base and source coverage alpha `as`, layer opacity p, mask m: `a=as*p*m`, `O=mix(B,F(B,S),a)`. Ordinary **Opacity** is not Photoshop **Fill**, which has special behavior for certain modes.

For partially transparent base alpha `ab`, source-over blend-mode compositing in premultiplied output form is:

`ao=a+ab*(1-a)`;
`co=a*(1-ab)*S + a*ab*F(B,S) + (1-a)*ab*B`;
`Co=co/ao` if `ao>0`, else zero.

This is the standard implementable compositing equation, not verified PS per-mode fixed-point internals. [W3C composition](https://www.w3.org/TR/compositing-1/#generalformula).

For normal 8/16-bit encoded-space composition use encoded B/S directly, quantize final writes. For gamma-1 normal composition decode document TRC, composite linear, re-encode. For 32-bit use native linear RGB and retain HDR values. How Photoshop's gamma preference distributes conversions between `F` and the alpha mix for each non-Normal mode is **UNCERTAIN**; the clean mathematical choice “convert both inputs, evaluate F, mix, convert back” must not be sold as universal PS parity.

## 10. Offset filter (`offset`) — Wrap Around versus Repeat Edge Pixels

These are **different modes**; “repeat wrap” is ambiguous. Adobe explicitly distinguishes [Wrap Around and Repeat Edge Pixels](https://helpx.adobe.com/photoshop/using/applying-specific-filters.html).

For integer offset `(dx,dy)` where positive dx moves content right and positive dy moves content down, source index is `(sx,sy)=(x-dx,y-dy)`.

- **Wrap Around:** `O(x,y)=I(modp(sx,W),modp(sy,H))`, `modp(k,n)=((k%n)+n)%n`. WGSL signed remainder needs this positive-modulo correction.
- **Repeat Edge Pixels:** `O(x,y)=I(clamp(sx,0,W-1),clamp(sy,0,H-1))`.
- **Set To Background:** sample source inside bounds; outside use the configured background color (alpha convention **UNCERTAIN**).

Use integer texture loads: no bilinear interpolation or color transfer is needed. Captured 8/16/32-bit offsets confirm positive dx right, positive dy down and `fillMode:repeat` means **Repeat Edge Pixels**, not wrap. On the 256² impulse, (-257,+513) produces all black at every depth; wrapping would retain an impulse. The GPU exposes `offset_repeat`; selection-relative bounds and fractional descriptor rounding remain unmeasured.

## Ripple (`ripple`), large-size measured calibration

**Measured on Photoshop 27.11.0:** 43 successful captures cover the original
8/16/32-bit impulses (amounts 1/100/500/999), RGB coordinate ramps at 256² and
512² at those amounts, natural images at amounts 100/500, and an independent
repeat of the 8-bit 256² amount-100 ramp. The repeat is decoded-pixel identical
(maximum difference zero); large ripple therefore uses a fixed phase, not the
GPU noise seed. Repeat determinism was directly measured at 8-bit only.

The previous cross-axis sinusoid with wavelength 32 pixels fails the coordinate
ramps, despite its misleading ~48 dB impulse-only aggregate. The supported
replacement is a **five-plane-wave approximation**, not an exact reconstruction
of Photoshop's proprietary internal waveform:

`O(p)=bilinear_clamped(I, p + (amount/100)*d(p))`, using integer pixel coordinates.
For each row below, let `t_x=2*pi*dot(p,f)/99` and
`t_y=2*pi*dot(p.yx,f)/99`; add
`sin((t_x,t_y))*s + cos((t_x,t_y))*c` to the displacement vector.

| f | s.x | s.y | c.x | c.y |
|---|---:|---:|---:|---:|
| (1,-2) | -1.85239733 | -0.74044822 | 0.86973454 | 1.90775758 |
| (2,-2) | 1.72207367 | -1.73872899 | -0.54525498 | 0.49029434 |
| (3,-6) | -0.91559535 | 1.84798225 | 1.65051777 | -0.39010454 |
| (0,2) | 0.00311832 | 0.73529035 | 0.73542606 | -0.01479289 |
| (3,-2) | -0.78852142 | -0.83165139 | 0.40380319 | -0.30525573 |

Fourier peaks and nonlinear fitting recover the shared 99-pixel spatial period;
linear least squares on the interior of the 512² float amount-999 ramp determines
these coefficients. This is a coordinate-dependent rule for arbitrary images,
not a corpus-ID lookup. At amount 100, measured component displacement reaches
approximately ±6.173 pixels. The field is independent of canvas dimensions:
the overlapping interior of 256²/512² float amount-100 ramps differs by at most
0.000031 pixel. Applying the coefficients to the independently captured
512² amount-100 ramp leaves 0.00726 pixel RMS/component; at amount 999 the
full-canvas clamped coordinate residual is 0.0669 pixel RMS, maximum 0.1131 pixel.

Expanded-corpus aggregate comparisons (max and RMSE in depth units; peak
255/32768/1) before → after:

| Depth | Cases | Max absolute | RMSE | PSNR dB |
|---|---:|---:|---:|---:|
| 8 | 15 | 255 → 13 | 43.515981 → 0.148561 | 15.358 → 64.693 |
| 16 | 14 | 32768 → 1654 | 5675.657620 → 7.838852 | 15.229 → 72.424 |
| 32 | 14 | 1 → 0.050483 | 0.173208 → 0.000238 | 15.229 → 72.453 |

Worst natural-image amount-500 PSNR is 54.415/59.268/59.272 dB at 8/16/32 bits.
At 16× difference amplification, the float natural-image residual concentrates
on the distorted terrain boundary and sun; the coordinate-ramp difference is
visually nearly black. Residuals remain from the fitted waveform, interpolation
and integer rounding, especially where sharp boundaries amplify subpixel
displacement error. Small-size ripple is **unmeasured** and retains its explicitly
approximate 8-pixel sinusoid. Negative amounts, transparent borders, other
Photoshop versions and canvases beyond these probes are not calibrated.

Measurement command:
`cargo run -p moya-calib --release -- compare --op ripple --dump --json /tmp/ripple-after.json`.
Reports are `/tmp/ripple-expanded-before.json` and `/tmp/ripple-after.json`;
raw difference TIFFs are `/tmp/moya-calib/ripple-*-diff.tif`. Visually inspected
16× PNGs: `/tmp/ripple-before-diff.png`,
`/tmp/ripple-32-natural-500-diff.png`,
`/tmp/ripple-32-coordinates512-999-diff.png`.
The focused runnable regression is `ripple_large_measured_coordinate_displacement`;
it was added but not executed during calibration.

## 11. Parity checklist: resolved versus unresolved

Concrete WGSL-ready mathematical kernels/transfers are provided for every requested operation, but exact proprietary parity is **not established** for blur kernels or several HDR cases. Public documentation alone cannot honestly satisfy an “exact Photoshop pixel math” assertion.

Additional empirical probes for unresolved behavior:

1. Gaussian: transparent inputs, selection-relative borders, intermediate radii, and integer subpass rounding.
2. Motion: nonintegral shear phases and extreme-distance canvas corners at integer depths.
3. Radial: additional repeated operations to characterize stochastic residuals and quality-specific sampling.
4. Levels/Exposure/HueSat: nonidentity endpoints, Colorize with nonzero lightness, and HDR excursions.
5. Blending: selected pixel pairs near 0, 0.5, 1 and HDR >1; opacity 0/25/50/100%; test integer quantization, gamma preference, Scale/Offset, alpha and Fill separately.

Record exact Photoshop build, document ICC profile, bit depth, gamma-blending preference, operation descriptors, and raw pixel output. Screenshots cannot resolve numeric parity or HDR values.

## 12. Smart Sharpen (`smartSharpen`) — measured operation settings

Photoshop 27.11.0 captures use `presetKindCustom`, `useLegacy=false`,
`noiseReduction=0`, `blur=lensBlur`, and
`shadowMode={amount:100%,width:52%,radius:1}`.
The four amount/radius pairs are `(50%,25.6/1300 px)`,
`(500%,256/1300 px)`, `(2500%,1280/1300 px)`, and
`(5000%,2560/1300 px)` at each depth.
The original impulse cases alone were misleading:
independent TIFF sample comparisons found all eight 8/16-bit captures exactly
equal to their inputs, while 32-bit peaks at the last three pairs were
2.3496666 / 4.8035913 / 5.1481285 with negative neighboring samples.
Clipping and full shadow protection hide the impulse response in integer
documents; this is not evidence that Smart Sharpen is an identity operation.
The 50% float capture is also exactly unchanged.

Twenty-four additional successful captures cover existing `natural` and
`sharpen-midtone` inputs at all depths. The latter is a 0.35/0.55 step plus
0.035-amplitude separable sinusoidal texture (periods 13 and 17 pixels)
and an offset +0.08 square. Natural/texture outputs are nonidentity for
the last three pairs; the 8-bit natural normalized RMS change is
0.009567 / 0.020265 / 0.022355. This rules out a broken/no-op capture.
`moya-calib gen` persists these 24 probes by cloning the original impulse
descriptors and retaining their ID suffixes; it also generates the midtone
input analytically. The 16-bit midtone input rounds to 0…32768 before
encoding as full-range TIFF samples, matching the capture preparation.
Focused generator assertions cover case counts, unchanged descriptors,
ID mapping and the midtone pattern bounds.

The GPU candidate now uses a constant-preserving 7x7 radial deconvolution
fit rather than Gaussian unsharp masking: sum each radial-class coefficient
times `(neighbor-center)` for squared distances
1,2,4,5,8,9,10,13,18, retaining source alpha. Coefficients are least-squares
fits to natural and midtone captures, with separate integer and float tables
at radii 256/1300, 5*256/1300, 10*256/1300 and linear interpolation between.
Integer output additionally applies a fitted per-channel
`smoothstep(0,0.25,center)` shadow taper and clips to [0,1]; float retains
signed overshoot. The candidate takes the actual operation depth explicitly.

**Approximation boundaries:** the compact, symmetric kernel does not reproduce
Photoshop's nonlinear tone adaptation or asymmetric outer ringing. The
radius<0.1 bypass, linear coefficient interpolation, amount capped at 500
for strength scaling, and endpoint hold beyond radius 1.96923 are candidate
policies, not independently established Photoshop parameter limits. Only
the exact coupled operation settings above are calibrated. General arbitrary
amount/radius pairs, wider radii, alpha handling in Photoshop, and alternative
blur/shadow/highlight descriptors are unresolved; no proprietary parity claim.

Calibration comparisons over 12 cases per depth (impulse, natural, midtone;
four amount/radius pairs each), with errors in native depth units:

| Depth | Before max / RMSE / PSNR dB | After max / RMSE / PSNR dB |
|---|---|---|
| 8 | 202.000015 / 34.689771 / 17.327 | 35 / 0.979698 / 48.309 |
| 16 | 25714.892578 / 4430.144612 / 17.381 | 12708 / 114.198241 / 49.156 |
| 32 | 43.799725 / 0.364824 / 8.758 | 0.619397 / 0.005796 / 44.737 |

Before was measured with the existing pre-edit release comparator binary;
after used `cargo run -p moya-calib --release -- compare --op smartSharpen
--dump --json /tmp/sharpen-after.json`. The initial attempted cargo baseline
was blocked by an unrelated in-progress `offset_repeat` caller migration.
The group PSNR criterion (40 dB) passes all depths, but individual float
natural 2500%/5000% cases remain 37.950/38.503 dB; the grouped pass does not
establish per-case parity. No unit tests, formatter, or standalone build ran.

Inspected `/tmp/smartSharpen-8-natural-00095-before-diff.png` and 8x-amplified
after previews `/tmp/smartSharpen-{16-natural-00381,32-natural-00668,
32-sharpen-midtone-00669}-after-diff-8x.png`. The old widespread texture
and clipped-edge errors collapse mostly to the high-contrast curved natural
edge and the midtone step/square boundaries. Fine residual texture, an
isolated bright spot, and missing wider/asymmetric ringing remain. Raw
float difference TIFFs are retained in `/tmp/moya-calib/*-diff.tif`; PNG
previews clip display range and cannot replace those numeric measurements.

## 13. Gaussian Add Noise (`addNoise`)

Photoshop 27.11 operation probes use Gaussian, color noise at amount 0.3%.
The previous uniform distribution was wrong even though random-pixel PSNR
was high. Independent TIFF residuals give channel standard deviations
0.7679/0.7708/0.7676 codes at8, 97.052/96.532/96.927 at16,
and .006008/.006002/.006004 in Float. The candidate uses Box–Muller Gaussian
noise with independent channels (or one shared sample for monochromatic),
preserves alpha, and keeps the seed reproducible without claiming PS seed parity.
Sigma is amount/100 at16, twice that in Float. At8 a fitted pre-rounding
factor .93145 matches the measured 0.3% output standard deviation;
extrapolation of that factor to other amounts remains uncalibrated.

| Depth | Before pixel max / RMSE / PSNR | After pixel max / RMSE / PSNR | Before / after statistics PSNR |
|---|---|---|---|
| 8 | 3.000015 / .968015 / 48.413 | 5 / 1.084871 / 47.423 | 65.972 / 92.000 |
| 16 | 421.847656 / 112.109835 / 49.316 | 630 / 137.513922 / 47.542 | 61.225 / 86.889 |
| 32 | .022799 / .006250 / 44.082 | .038042 / .008483 / 41.429 | 50.393 / 81.803 |

Uncorrelated Gaussian realizations increase diagnostic pixel error relative to
the old low-variance uniform candidate; this is not a distribution regression.
After channel standard deviations are .7675/.7617/.7655, 98.137/97.712/97.912,
and .006007/.005978/.005992 respectively. PS's16-bit residual kurtosis is
about2.82 rather than an ideal Gaussian's3, and captured tails are
bounded near ±3.33 sigma. The candidate retains Gaussian tails instead of
claiming reconstruction of that proprietary finite sampling table.
The corpus does not yet establish monochromatic cross-channel behavior.

Measured with `cargo run -p moya-calib --release -- compare --op addNoise
--dump --json /tmp/noise-after.json`; all residual-statistics criteria pass.
Inspected amplified float diff TIFF previews: spatially unstructured color
grain, with no tone-curve or edge-pattern residual. Focused numeric checks
cover centered mean, per-depth sigma, Gaussian fourth moment, seed repeatability,
and alpha preservation; they were added but not run by this worker.

An additional isolated 0.3% Gaussian noise probe gives **47.453 dB**
pixel PSNR but **88.540 dB** residual-statistics PSNR; rounding both
independent realizations to bytes gives **46.864 dB**. Independent random
realizations therefore cannot establish a deterministic 50 dB pixel match.
Separate depth-conversion probes for 8→32 and 32→16 give 136.400/88.000 dB
(max 7.46e-6 Float / three 16-bit codes).
Other isolated operation probes report Float gamma 2.2 maximum error
3.79e-4, Gaussian radius 0.9765625 maximum error 0.009545 (86.799 dB),
and gamma 0.45 PSNR 155.099 dB. An exact-input Float Linear Dodge probe
is bit-exact. These results apply only to the measured inputs and settings.

## 14. ACR Highlights -100 (`Adobe Camera Raw Filter`)

Captured `$PrVN:6,$PrVe:251920384,$Hi12:-100` through serialized
`calib/ps-run.sh`. Nine added cases extend the original three: each depth has
the existing synthetic natural landscape, its brighter RGB-reversed variant
`clamp(1.45*reverseRGB+.03)`, a horizontally rolled .65-scaled variant, and
the existing neutral ramp. All twelve captures succeeded, with no persisted
Photoshop preference edits or open documents. These are synthetic landscape
variants, not a representative photographic test set or independent holdout.

The candidate is a fitted local tone/chroma approximation, **not ACR parity**.
Let `l=clamp(.30R+.59G+.11B,0,1)` and `s=max(R,G,B)-min(R,G,B)`.
Build a two-channel guide `(l,s)` and blur it with radius64; call those channels
`b,q`. A sixth-degree zero-intercept neutral curve is fit to the ramp.
A ten-term polynomial on `(l,l²,s,s²,b,q,l*s,l*b,b*q,q²)` is independently
fit to the three landscape captures. Mix neutral and chromatic losses by
`min(2*q/max(b,.01),1)`, and constrain loss to `[0,.7]`. Local chroma rather
than individual pixel saturation is necessary: a neutral isolated sun in a
colored scene does not receive the same recovery as the white end of a gray ramp.
Integer output subtracts this scalar loss from RGB and clips negative channels;
Float output multiplies RGB by `1-loss`. This preserves source alpha.

The integer common subtraction and Float common scale are measured
approximations: independently optimal per-pixel affine color fits leave
about .0017 RGB RMS at16 versus .00001 at32. Photoshop's integer channel
response, local edge-aware decomposition, and highlight reconstruction
are not fully reproduced. The two polynomial coefficient sets live in
case24; they fit all the cases above, not an independent validation set.
The local guide uses one extra full-frame dispatch plus the existing bounded
large-blur pyramid. The image-adaptive correction below adds a reduced guided
map and GPU key reduction, with no CPU readback or image-wide host fitting.

| Depth | Expanded before max / RMSE / PSNR | Expanded after max / RMSE / PSNR |
|---|---|---|
| 8 | 110 / 18.561214 / 22.759 | 21 / 1.899936 / 42.556 |
| 16 | 14142 / 2381.288231 / 22.773 | 2740 / 235.887523 / 42.855 |
| 32 | .350875 / .074207 / 22.591 | .128391 / .005331 / 45.464 |

Every case exceeds40dB, not only the grouped score. Primary landscape before/
after PSNR is33.063→42.235,32.841→43.070,29.017→46.175 by depth.
The worst landscape cases after are41.306dB at8,41.079dB at16,
and42.353dB in Float. Ramp results are47.081/48.078/49.762dB.
Residual maxima remain large at isolated spots and terrain boundaries,
despite low area-weighted RMS. Larger images, other scene distributions,
fully neutral photographic scenes, and HDR outside0…1 are not calibrated;
the input-luminance clamp and loss bound prevent polynomial extrapolation
from inventing an HDR recovery rule.

Measured with `cargo run -p moya-calib --release -- compare --op
'Adobe Camera Raw Filter' --dump --json /tmp/acr-after.json`; all case and
group criteria pass. Expanded baseline is `/tmp/acr-before.json`.
Inspected source/oracle/output and 8x-amplified diff TIFF previews in
`/tmp/moya-calib`, including primary16, bright16/32 and dark32.
The former widespread bright-sky loss is removed; remaining differences
outline the terrain and sun, with small broad sky/foreground tone shifts.
The neutral-curve regression uses excerpts of measured PS ramp outputs
with .02 normalized tolerance, not polynomial-pinned expected values.
No tests or formatter were run by this worker.

### Full-image operation measurements

The crop fit alone generalized poorly to mostly black scenes. Standalone ACR
captures are retained under `reference-corpus/acr-replay`: natural256×256,
Embers1086×1448, and synthetic256×192, all at8-bit.
All three captures succeeded through `ps-run.sh`.
The synthetic ACR oracle has maximum RGB26/255 despite source values up to1;
the gray ramp's white remains237/255. Thus an absolute local shoulder cannot
describe both inputs. This is image-relative recovery.

The additional recovery map uses linear-sRGB Rec.709 luminance:
1. Reduce linear RGB bilinearly to at most512px on the long edge, without upscaling.
   Integer input decodes each of the four taps before interpolation in one dispatch;
   there is no full-resolution linear-RGB intermediate allocation.
2. `L=log2(max(.2126R+.7152G+.0722B,.0006))`; image key is the mean of L.
3. Self-guided box filter with radius `max(1,round(.003*longEdge))` and epsilon8:
   `a=variance(L)/(variance(L)+8)`, `b=(1-a)*mean(L)`, then box-average a,b.
4. Bilinearly sample the coefficients at original pixel centers and evaluate
   `z=a*log2(max(originalLinearY,.0006))+b-key`.
5. Linearly interpolate correction stops at z4…9:
   `[0,-1.99206661,-2.38074202,-2.90063828,-4.34677746,-7.83526378]`,
   clamping both endpoints. Multiply the previous candidate in linear RGB
   by `exp2(stops)`, then encode integer-depth output.

This is a measured, image-relative correction, with no image-ID dispatch.
The plateau below four relative stops preserves the original landscape/ramp
fit while the same curve suppresses highlights above a dark image key.
The monotone high-end clamp avoids extrapolating an underdetermined tail.
Mean-log key and radius/epsilon were fitted on these full-image captures,
not established as Adobe constants. The GPU mean is reduced by bounded
half-size bilinear passes; non-power-of-two dimensions approximate the mean.
The exact-input CPU correction gives RGB PSNR42.235/38.689/43.518dB:
Embers retains localized bright-detail residuals. This does not establish
photographic ACR parity.

Final GPU full-image ACR comparison,8-bit native units:

| Source | Before max / RMSE / PSNR | Adaptive max / RMSE / PSNR |
|---|---|---|
| Natural | 21 / 1.971436 / 42.235 | 21 / 1.971436 / 42.235 |
| Embers | 191 / 10.869978 / 27.406 | 141 / 2.961949 / 38.699 |
| Synthetic | 220 / 16.832847 / 23.608 | 50 / 1.648532 / 43.789 |

The standalone Embers ACR case remains below40dB and its corpus comparison
therefore correctly fails; the remaining bright-detail/color residual was
inspected, not hidden by an acceptance override. The twelve earlier crop cases
all still pass, at grouped42.556/42.855/45.464dB for8/16/32.

Evidence: `/tmp/acr-replay-{before,after}.json`,
`/tmp/acr-crops-adaptive-after.json`, and full-image inputs/oracles in
`reference-corpus/acr-replay`. Operation-comparison commands:
`target/release/moya-calib compare --root ../reference-corpus/acr-replay
--dump --json /tmp/acr-replay-after.json` and
`target/release/moya-calib compare --op 'Adobe Camera Raw Filter' --dump
--json /tmp/acr-crops-adaptive-after.json`. Inspecting 8x full-image and
worst-pixel diff previews shows residual bright Embers detail and synthetic
bar/spot edges.
