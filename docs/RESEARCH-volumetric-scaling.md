# Scaling the Volumetric Renderer to Thousands of Pixel-Mapped Emitters

> Companion to `docs/RESEARCH-volumetrics.md` (the froxel architecture spec) and the
> Phase-0 perf report. This doc is the **field-grounded scaling strategy**: it takes
> the verified literature on many-light + volumetric rendering, maps each technique to
> *this* renderer, and lays out a staged plan from "ship today" to "real rewrite."
> Sources are cited with URLs and were adversarially fact-checked (23/25 claims
> confirmed; 2 ReSTIR over-reach claims killed). wgpu/Metal feasibility was verified
> **empirically on the target Apple M5 Pro**, not assumed.

## 0. The one-sentence answer

The renderer's cost is `O(halfPixels × steps × fixtures × taps)` — **strictly linear in
light count**, and pixel-mapping will push light count into the thousands. The fix is to
**stop evaluating every light at every pixel-step**. Two structural moves do that, in
order of payoff/effort: **(B) GPU light culling** so each ray-step only touches the few
lights near it, then **(C) a froxel grid** that evaluates lighting *once per 3D cell*
instead of once per pixel-step — with the existing high-res raymarch demoted to a
"hero pass" for the handful of sharp moving-head beams. Pixel-mapped LEDs (wide, dim,
low-frequency) are the *ideal* input for a coarse froxel grid; sharp gobo/CA/prism beams
are not, which is exactly why the split exists.

## 1. Measured cost model (this codebase, today)

Isolated by toggling fog on/off on the 146-fixture Basic Festival @2560×1600:

| Pass | Cost | Scales with |
|---|---|---|
| Volumetric raymarch | **~70% of frame** | pixels × steps × **fixtures** × taps |
| Forward (floor/stage/fixtures) | ~25% | fragments × **lights** (mesh.wgsl loops every light too) |
| Bloom + tonemap + readback | fixed (~overhead) | resolution |

Two facts this nails down:
1. **The volumetric inner loop is the wall**, but **the forward mesh pass also loops every
   light per fragment** (`mesh.wgsl` floor-pool). A complex stage with only ~10 fixtures
   still tanks because fragment-count × light-count. **Any culling solution must feed both
   passes** — they already share `FixtureGpu` + `optics.wgsl`, so a shared light-cluster
   structure feeds both for free.
2. The cost is linear in fixtures. At 10→150 fixtures it's painful; at thousands of
   pixel cells it's hopeless. This is an **algorithmic** problem, not a constant-factor one.

## 2. Verified feasibility on the target HW (Apple M5 Pro / Metal / wgpu 29)

The literature couldn't answer "does this work in wgpu 29 on Apple Silicon?" — so I probed
the live adapter. Results (better than the generic-WebGPU assumptions):

- **Compute shaders**: available (core). The repo's first compute pipelines are unblocked.
- **`rgba16float` as a storage texture**: available via `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`
  (which the adapter reports as supported). Enable that one feature at device creation.
- **`rgba16float` read-write storage = `true`** on this GPU. → **No ping-pong needed** for
  the froxel V-buffers on Apple Silicon. This *corrects* `RESEARCH-volumetrics.md` §3.1,
  which assumed ping-pong is mandatory — that's a generic-WebGPU limitation that does **not**
  apply here. (Keep a ping-pong fallback only if you later target Vulkan/DX12/web, where
  rgba16float read-write isn't guaranteed.)
- Limits: 3D textures to 2048³, 128 storage textures/stage, 1024-invocation workgroups,
  31 storage buffers/stage — all ample for a 160×90×128 froxel grid + cluster light lists.

**Verdict: the froxel + compute + clustered-culling architecture is fully buildable on this
machine today.** The only required device change is enabling `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`.

## 3. The staged roadmap

### Phase A — lossless quick wins (DONE / shipping)
- **Per-fixture radial pre-cull, CA-tap collapse, constant-dt cap** (Phase-0). Lossless,
  ~3.7× on the festival. Linear in lights still — buys headroom, not scale.
- **Fog box on the floor** (done this pass): default box was centered at origin → buried
  10 m under the floor; now `min.y = 0`. Kills below-floor beams + sub-floor marching.
- **Still cheap & worth doing**: clamp the fog AABB tighter to actual fixture/beam bounds;
  per-tile (screen) depth min/max to shorten rays to the occupied depth range. O(1) changes,
  zero fidelity risk. *(Clustered shading's documented strength is "avoiding empty space" —
  Olsson HPG 2012.)*

### Phase B — GPU light culling feeding the EXISTING raymarch (the pragmatic next step)
**This is the highest value-per-effort move and does NOT require the froxel rewrite.** Build a
clustered light structure on the GPU and have both the raymarch and the mesh pass loop only
the lights in the current cell instead of `0..arrayLength(&fixtures)`.

- **Technique**: clustered shading (Olsson/Billeter/Assarsson, HPG 2012) with **exponential
  view-space depth slices**, or the **CoD-2017 / Granite XY-Z-decoupled Z-binning**
  (`O(X·Y + Z)` storage instead of `O(X·Y·Z)`; lights sorted by Z, a 1D LUT of min/max light
  index per Z-bin). Granite's author **explicitly validated cluster culling inside volumetric
  fog**, so it's correct for a full-depth raymarch (not just opaque shading) — this was the
  load-bearing verification.
- **Scaling**: light *assignment* is sub-linear (Olsson Table 2: 32→1M lights ≈ 0.71→5.73 ms,
  ~8× for 32,768×). **Caveat (verified):** only *assignment* is sub-linear — per-cell *shading*
  stays `O(lights-touching-that-cell)`. For a spatially-spread rig that's the difference
  between O(all fixtures) and O(local density) — decisive. For a tightly-packed LED panel
  where hundreds of cells fall in one froxel, the worst case must be **measured**, and is the
  motivation for aggregate-emitter LOD (Phase D).
- **Maps to us**: a `clusterLights: array<u32>` index list + a per-cell `{offset,count}` (or
  CoD Z-bin LUT), built in a small compute pass each frame; the raymarch computes its cell
  from world/clip pos and loops that slice. Feeds `mesh.wgsl` too (shared structure → floor
  pool and forward fixtures cull identically, preserving lock-step).
- **Payoff**: breaks the O(all-lights) inner loop → roughly **the average-light-list-length
  speedup** (large for spread rigs; modest where everything overlaps one cell). **Visual
  risk: none** (it's exact — only skips lights that contribute zero; validate by diffing
  against culling-off). **Effort: M** (first compute pass + buffers + bind groups).
- **Why before froxels**: smaller, exact, reuses the whole existing optics chain, and the
  cluster structure is *also* the input froxel injection needs in Phase C — so it's not
  throwaway work.

### Phase C — froxel volumetrics: the architectural endgame for the masses
Replace per-pixel-per-step light evaluation with a frustum-aligned 3D grid evaluated **once
per cell**. This is what truly **decouples light cost from pixel count**.

- **Technique**: Wronski (Assassin's Creed 4, SIGGRAPH 2014) + Hillaire (Frostbite, SIGGRAPH
  2015): `inject` (compute, one thread/froxel, loops only the cell's clustered lights, writes
  scatter+extinction into a 3D `rgba16float`) → `integrate` (compute, marches +Z accumulating
  the **same** `(1−exp(−σ·dt))/σ` slice integral the renderer already uses) → fragment does
  **one trilinear lookup**. Store **extinction (linear), not transmittance** — this is what
  makes temporal accumulation valid (directly answers our "store optical depth" concern).
- **Directly portable reference**: `diharaw/volumetric-fog`
  (https://github.com/diharaw/volumetric-fog) — two compute passes, **byte-for-byte the same
  slice integral**, 160×90×128 grid, exponential `slice = n·pow(f/n,(z+0.5)/Z)` depth
  distribution, blue-noise jitter, temporal reprojection, trilinear lookup. MIT-ish, ~140★,
  last pushed 2024. **Caveat**: it injects only *one* directional light — it proves the
  *pipeline shape*, not the many-light decoupling, which is exactly why Phase B (clustered
  culling) is the load-bearing partner.
- **Scaling**: light eval moves from `pixels×steps` (≈ millions) to `froxels` (≈ 1.2M for
  160×90×128, but evaluated once, and trilinearly shared by all pixels). With Phase-B culling
  inside `inject`, each froxel loops only nearby lights. **This is the move that makes
  thousands of pixel-mapped LEDs affordable.**
- **Visual risk: HIGH if used alone.** A 160×90×128 grid is far coarser than the current
  half-res raymarch (≈960×540). It cannot carry sharp super-Gaussian cone edges, gobo
  cutouts, CA fringe, or prism-separated aerial beams — those fall below the grid's Nyquist.
  **Mitigation is Phase C.2, non-negotiable.**
- **Effort: L** (first compute pipelines, 3D storage textures, feature flag, exp-Z mapping,
  composite rewrite). Feasibility confirmed in §2.

#### C.2 — Hero-beam hybrid (the fidelity-preservation half)
Keep the **existing high-res per-fixture raymarch** for the *few* sharp on-screen "hero"
moving-head beams (gobo/CA/prism), composited **over** the froxel fog (`L = L_froxel +
L_beam·Tr_froxel`); inject the **thousands of wide/dim pixel-mapped LEDs** into the froxel
grid (they're low-angular-frequency — ideal for a coarse grid). Rank heroes by cone
narrowness × screen area × proximity; **cross-fade**, never hard-cutoff, or beams pop on
orbit. Exclude heroes from froxel injection to avoid double-counting. *(This split is a sound
inference from the literature — froxel grids are confirmed coarse vs a half-res raymarch — but
no source benchmarks it for DMX previz; the seam/cost trade-off must be prototyped.)*

### Phase D — extreme scale (only if Phase B+C aren't enough)
- **Aggregate-emitter LOD for dense pixel panels** (the real pixel-mapping endgame): when
  hundreds of LED cells fall in one froxel/cluster, don't loop them — **merge them into one
  area/emissive contribution** (the panel's summed radiance), or inject pixel emitters as
  **emissive into the froxel grid directly** (light-as-data) rather than as lights to iterate.
  This is how production tools treat pixel walls. Bounds the per-froxel worst case that
  clustered culling alone doesn't.
- **Light-tree importance sampling** (Estevez & Kulla, HPG 2018,
  https://fpsunflower.github.io/ckulla/data/many-lights-hpg2018.pdf): O(N)→~O(log N) per
  sample via one BVH over all emitters. **Tiled Light Trees** (Yakovlev/EA+AMD, I3D 2017,
  https://www.kayru.org/publications/TiledLightTrees-preprint.pdf) is the directly-relevant
  hybrid: flat list per froxel for dense moving-head clusters, tree for froxels holding
  hundreds of small LEDs. **Caveat**: original is offline Monte-Carlo path tracing —
  stochastic/noisy, only viable here *with* temporal reuse.
- **Volumetric ReSTIR** (Lin et al., SIGGRAPH Asia 2021,
  https://dqlin.xyz/pubs/2021-sa-VOR/): 1 spp matching 4–13 spp quality. **Research horizon,
  not a near-term option** — every verified deployment is an RTX/DXR voxel path tracer
  (55–142 ms/frame on an RTX 3090). Two "real-time millions of lights" ReSTIR claims were
  **refuted** in verification for overstating drop-in applicability. Harvest its *cheap* ideas
  now (blue-noise jitter + temporal accumulation of extinction), which the diharaw froxel
  reference already demonstrates.

## 4. Temporal & upsample (orthogonal, applies to B/C)
- **Temporal accumulation** of extinction (not transmittance) + blue-noise jitter can amortize
  steps across frames — but for *this* app (orbit-heavy camera, fast moving heads, strobes,
  scrolling 3-octave haze) it ghosts badly without a neighborhood-variance clamp +
  disocclusion fallback. **Prototype cautiously; froxels make it 3D and more robust than the
  2D screen-space version.** De-prioritized until froxels exist.
- **Half-res + depth-aware bilateral upsample** (we already do half-res). Improve the
  composite to a nearest-depth/bilateral tap (pixelmager:
  https://gist.github.com/pixelmager/a4364ea18305ed5ca707d89ddc5f8743; Lords of the Fallen
  volumetric talk) to protect beam-vs-geometry edges. Cheap, low risk.

## 5. Corrected assumptions (things the research changed)
- ❌ "rgba16float needs ping-pong on WebGPU" → ✅ **read-write rgba16float works on Apple
  Silicon** (probed). Simplifies the froxel rewrite. (Keep ping-pong only for portability.)
- ❌ "clustered culling is O(log N) shading" → ✅ only *assignment* is sub-linear; **per-cell
  shading stays O(lights-in-cell)**. Dense pixel panels are the worst case → Phase D LOD.
- ❌ "ReSTIR makes thousands of lights cheap in real time, drop-in" → ✅ **false for our
  renderer**; it's an RTX path-tracer technique. Don't chase it yet.
- ✅ Confirmed: screen-*tile* (2D) binning straddles depth discontinuities; the **3D cluster
  depth dimension is the correct fix for a full-depth raymarch**, and clustered culling is
  validated *inside* volumetric fog.

## 6. Prototype-and-measure (open questions no paper answers for us)
1. **Per-froxel worst case for dense pixel-mapping**: how many lights pile into one froxel for
   a packed LED panel, and does clustered culling still pay or do we need aggregate LOD
   immediately? Instrument the cluster build and log max/avg list length on a real pixel rig.
2. **Compute vs fragment on TBDR**: the probe proves compute works; whether the froxel compute
   path beats the current tile-based fragment raymarch on Apple Silicon must be **A/B'd**
   (`PREVIZ_BENCH`). Tile memory may already favor the fragment path for the integrate step.
3. **Hero/froxel composite seams**: where a hero beam crosses froxel fog — bilateral upsample
   + blue-noise jitter to hide the resolution boundary; verify no visible seam on orbit.
4. **Temporal under strobes**: does extinction-accumulation + variance clamp survive strobes +
   fast pan/tilt, or ghost? Gate temporal off for flagged strobe frames if so.

## 7. Recommended order of work
1. **Now**: finish Phase A empty-space wins (tighter AABB already partly done; add per-tile
   depth min/max). Cheap, safe.
2. **Next (biggest ROI without a rewrite)**: **Phase B clustered light culling** feeding both
   the raymarch and the mesh pass. First compute pass; exact, no fidelity risk; the cluster
   structure is reused by Phase C.
3. **Then (the real rewrite)**: **Phase C froxel inject→integrate + hero-beam hybrid.**
   Decouples light cost from pixels; makes pixel-mapping affordable. Use `diharaw/volumetric-fog`
   as the reference; enable `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`; no ping-pong needed here.
4. **At scale**: **Phase D aggregate-emitter LOD** for dense panels; temporal; (light-tree /
   ReSTIR only if profiling demands).

## References (verified, primary unless noted)
- Hillaire, *Physically Based & Unified Volumetric Rendering in Frostbite*, SIGGRAPH 2015 —
  https://www.slideshare.net/DICEStudio/physically-based-and-unified-volumetric-rendering-in-frostbite
  (extinction-not-transmittance; 8×8×64 froxels; decoupled inject/integrate).
- Wronski, *Volumetric Fog* (Assassin's Creed 4), SIGGRAPH 2014 —
  https://bartwronski.com/2014/08/20/major-c-net-graphics-framework-update-volumetric-fog-code/
- `diharaw/volumetric-fog` — https://github.com/diharaw/volumetric-fog (portable froxel
  reference; same slice integral; exp-Z; blue-noise; temporal).
- Olsson, Billeter, Assarsson, *Clustered Deferred & Forward Shading*, HPG 2012 —
  https://www.highperformancegraphics.org/previous/www_2012/media/Papers/HPG2012_Papers_Olsson.pdf
- Arntzen, *Clustered Shading Evolution in Granite*, 2020 —
  https://themaister.net/blog/2020/01/10/clustered-shading-evolution-in-granite/ (XY-Z
  decoupled binning; validated *in volumetric fog*).
- Drobot, *Improved Culling for Tiled and Clustered Rendering* (CoD: Infinite Warfare),
  SIGGRAPH 2017 (Z-bin LUT, sorted lights).
- Estevez & Kulla, *Importance Sampling of Many Lights…*, HPG 2018 —
  https://fpsunflower.github.io/ckulla/data/many-lights-hpg2018.pdf (light BVH, O(log N)).
- Yakovlev et al., *Tiled Light Trees*, I3D 2017 —
  https://www.kayru.org/publications/TiledLightTrees-preprint.pdf (tree-vs-list hybrid).
- Lin et al., *Volumetric ReSTIR*, SIGGRAPH Asia 2021 — https://dqlin.xyz/pubs/2021-sa-VOR/
  (research horizon; RTX path tracer).
- Ortiz, *A Primer On Efficient Rendering Algorithms & Clustered Shading* —
  http://www.aortiz.me/2018/12/21/CG.html
- ARM, *Clustered Volumetric Fog* (mobile TBDR) —
  https://developer.arm.com/community/arm-community-blogs/b/mobile-graphics-and-gaming-blog/posts/clustered-volumetric-fog
- Epic, *DMX Previs Sample / Pixel Mapping in Unreal* —
  https://dev.epicgames.com/documentation/en-us/unreal-engine/dmx-previs-sample-project-for-unreal-engine
  · https://docs.unrealengine.com/5.1/en-US/dmx-pixel-mapping-in-unreal-engine/
- grandMA3 render quality — https://help.malighting.com/grandMA3/2.1/HTML/patch_render_quality.html
  · Depence² — https://www.syncronorm.com/products/depence2/visualization/lighting
- Apple, *Harnessing Apple GPUs with Metal* (TBDR/tile memory) —
  https://developer.apple.com/videos/play/wwdc2020/10632/
