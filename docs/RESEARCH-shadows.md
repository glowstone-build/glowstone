# Shadows for Many Lights + Volumetric Haze — Applied Plan

> Companion to `docs/RESEARCH-volumetric-scaling.md`. Field-researched (peer-reviewed
> primary sources + UE5 docs, adversarially verified: 22/25 claims confirmed, 3
> over-reach claims killed) and **hardware-feasibility-probed on the target Apple M5
> Pro**. Answers: how to shadow the *thousands* of pixel-mapped LED/wash emitters, the
> *dozens* of sharp moving-head beams, and the *volumetric haze* — and corrects the
> earlier wrong claim that "washes don't need shadows."

## 0. TL;DR

"Washes don't need shadows" was **wrong** — soft many-light shadows are real (it's why
beams currently pass through solid set pieces), they're just *cheaply approximable*. The
plan splits by regime, because **no technique gives accurate per-light shadows for
literally thousands of casters** — the proven real-time ceiling for per-light shadow maps
is *hundreds* (Olsson clustered shadows; UE5 VSM), so the masses must be approximated:

| Regime | Best technique | Scaling | Portable (wgpu raster)? |
|---|---|---|---|
| **Dozens of sharp moving-head beams** | dedicated **per-beam shadow maps** (high-res) | linear in *hero* count (bounded ~8–24) | ✅ yes — ship first |
| **Thousands of static LED panels/bars** | **shadow atlas + frame-to-frame caching** (re-render only changed) | sub-linear (cached) | ✅ yes |
| **The volumetric haze (self-shadow + beams occluded mid-air)** | **per-froxel transmittance-to-light** in the planned froxel grid | light-count-**independent** | ✅ yes (compute) |
| **Everything, grounding contact** | **screen-space contact shadows** | O(1) per pixel | ✅ yes — cheap global add |
| **The masses, accurately, at scale** | **stochastic (MegaLights/ReSTIR) + ray tracing** | count-independent | ⚠️ Mac-native only (see §4) |

**One structure serves all three:** the froxel/cluster grid we're already planning is a
"fast voxelization of the visible geometry" (Olsson TVCG 2015) — it does clustered light
culling **and** shadow-caster culling **and** carries per-froxel volumetric visibility.

## 1. The masses (thousands of pixel-mapped LEDs/washes)

**Reality check (verified):** per-light virtual/cached shadow maps — UE5 Virtual Shadow
Maps, Olsson clustered virtual cube maps — are state-of-the-art but **scale to hundreds,
not thousands** of shadow-casters in real time (Olsson benchmarks peak ~376 lights @34ms
on a Titan; the "thousands" figures are *non-shadowed* clustered shading). So we cannot
give every LED a real map.

**What works:**
- **Atlas + caching, split static/dynamic.** Cached pages/maps are cheap *only for static
  lights*; a moving light invalidates all its pages and pays a full re-render (~8–16×)
  (Epic UE5.7 VSM docs; StraySpark: cached local VSM ~0.05 ms vs ~0.4–0.8 ms invalidated,
  "animated lights: never VSM"). → **Pixel-mapped LED panels/bars are mostly static →
  caching-friendly.** Render their shadow maps into a software-managed atlas/texture-array
  pool (we have **2048 array layers**, probed) and re-render only the few that change.
- **Imperfect Shadow Maps (ISM)** for the long tail: crude low-res maps whose per-light
  error *averages out* because the masses are dim/soft/overlapping (Ritschel 2008). ⚠️
  **Caveat (verified):** the "render hundreds of ISMs in one cheap pass" claim is
  over-stated — ISM still renders a crude map *per light*; it lowers per-light cost, it is
  **not** light-count-independent. And bright, individually-controllable stage washes are a
  worse fit for averaging than the dim diffuse VPLs ISM assumes.
- **Selection + fallback** (EG2025, Zhang/Lin/Wyman/Yuksel): full-res maps for a small
  *selected subset* of dominant lights (10–20, fixed budget) + crude maps for the rest.
  The selection in the paper uses ReSTIR/ray tracing; we'd approximate it with a
  clustered/screen-space *importance heuristic* in a compute pass (unproven for raster —
  prototype it).
- **Cheap global proxies** that don't shadow per-light but ground the scene: **screen-space
  contact shadows** (short ray-march in the depth buffer — UE5 Contact Shadows) and
  **GTAO/bent-normals** (XeGTAO) for ambient many-light occlusion. Cheap, portable, hide a
  lot of "light passes through everything."

**Best single pick for the masses (portable):** static **shadow-atlas + caching** for the
LED panels (they barely move) + **screen-space contact shadows** for grounding. Add ISM /
selection only if the look still needs per-LED shadows.

## 2. The haze (volumetric self-shadow + beams occluded mid-air)

**Best fit (verified), and it's free-ish once froxels exist:** store **transmittance-to-
light per froxel** inside the Wronski/Hillaire inject→integrate grid. The froxel raymarch
runs **once per froxel, independent of light count** (Wronski 2014: "raymarching just once,
independent of number of light sources"), so the same structure gives haze self-shadowing
*and* geometry-occludes-beam-mid-air. This is the single best haze answer and reuses the
grid we're already building.

If a separate light-view volumetric-shadow structure is wanted (e.g. for a hero beam):
- **Fourier Opacity Maps (FOM)** — transmittance as a truncated Fourier series, **fixed
  16-coeff cost independent of depth complexity**, single additive-blend raster pass →
  **most portable to wgpu**; scoped to smooth low-opacity media (our 3-octave noise fog
  qualifies; watch for ringing) (Jansen & Bavoil I3D 2010).
- **Adaptive Volumetric Shadow Maps (AVSM)** — bounded streaming curve compression, higher
  quality, but **DX11 pixel-sync/ROV origins → portability to wgpu/Metal is uncertain**.
  FOM is the safer first bet (Salvi EGSR 2010).
- ❌ *Not* "Frostbite uses one simple volumetric shadow map for any light" — that claim was
  **refuted**; Frostbite stores extinction per froxel and culls lights, it doesn't share one
  map.

## 3. The hero beams (dozens of sharp moving heads) — ship this first

Crisp gobo/CA beam edges **need accurate visibility** that ISM/averaging can't resolve
(Ritschel: "crisp indirect shadows cannot be reproduced with low-res ISMs"). And moving-head
beams are *animated* → un-cacheable → they correctly warrant **dedicated per-beam high-res
shadow maps**, exactly as the architecture already planned. This is the **first, portable,
highest-value** shadow feature:
- Render scene depth from each hero beam's POV (perspective matching the cone) into a
  shadow atlas layer; cap at N (~8–24) ranked by cone-narrowness × screen-area × brightness;
  cross-fade in/out (no popping).
- Sample in **`mesh.wgsl`** (surfaces cast shadows on floor/each other) **and**
  **`volumetric.wgsl`** (the *shaft* gets occluded mid-air by geometry) with PCF
  (`Depth32Float` is filterable — probed).
- Washes/pixels skip this pass.

## 4. Feasibility on the target HW (probed Apple M5 Pro / Metal / wgpu 29)

- ✅ `Depth32Float` / `Depth24Plus`: `TEXTURE_BINDING` + **filterable** → comparison/PCF
  shadow sampling works.
- ✅ **2048 texture-array layers** + `TEXTURE_BINDING_ARRAY` + `PARTIALLY_BOUND` +
  non-uniform indexing → a big bindless **shadow atlas/pool** is feasible (software-emulate
  VSM's page-on-demand; the hardware sparse-texture path the academic VSM papers need is
  **not** in wgpu, and didn't even hit real-time in the original paper).
- ✅ `R16Float`/`R32Float` read-write storage, 3D textures to 2048³ → froxel visibility,
  variance/exponential shadow maps, scene SDF all feasible in compute.
- ⚠️ **`EXPERIMENTAL_RAY_QUERY` IS reported** by the adapter (hardware ray tracing via
  Metal). This *unlocks the stochastic path* — **UE5 MegaLights** (SIGGRAPH 2025: stochastic
  direct lighting + ray-traced visibility + ReSTIR + denoise) is the shipping answer to
  "thousands of *shadowed* lights" and would serve masses **and** haze with one ray budget,
  count-independent. **But it's experimental in wgpu and non-portable** (no web/other-backend
  guarantee) and a major build (BVH management, denoiser). Treat as a **Mac-native fast-path
  to evaluate later**, not the first move. `EXPERIMENTAL_MESH_SHADER` + `MULTIVIEW` are also
  available (multiview can render several shadow views in one pass).

## 5. Staged plan

1. **Hero-beam shadow maps** (portable, biggest visible win, fits existing architecture):
   per-beam depth atlas, sampled in `mesh.wgsl` + `volumetric.wgsl`. **← start here.**
2. **Screen-space contact shadows** — cheap global grounding for *all* lights (masses
   included); hides most "light through geometry" instantly.
3. **Froxel grid (from the scaling doc) + per-froxel transmittance-to-light** — haze
   self-shadow + beam-by-geometry occlusion, light-count-independent. Lands with the froxel
   rewrite.
4. **Static-fixture shadow atlas + caching** — real per-LED-panel shadows for the static
   masses, re-rendering only changed maps.
5. **(Evaluate) stochastic MegaLights/ReSTIR + RT** — Mac-native fast-path for accurate
   shadowed thousands; only if 1–4 leave a gap and the experimental RT path proves stable.

## 6. Corrections to earlier assumptions
- ❌ "Washes don't need shadows" → ✅ they do; the fix is **cheap approximate** shadows
  (atlas+cache / contact / froxel visibility / ISM), not skipping.
- ❌ "No hardware RT on this target" → ✅ `EXPERIMENTAL_RAY_QUERY` is present (experimental,
  non-portable) — a real fast-path option.
- ❌ "VSM/ISM make per-light shadows for thousands feasible" → ✅ proven ceiling is
  *hundreds*; thousands must be approximated/cached/stochastic.

## References (verified, primary unless noted)
- Olsson et al., *Efficient Virtual Shadow Maps for Many Lights* / *Clustered shading*,
  HPG/TVCG 2014–15 — https://www.cse.chalmers.se/~uffe/ClusteredWithShadows.pdf ·
  https://www.cse.chalmers.se/~d00sint/more_efficient/clustered_shadows_tvcg.pdf
- Epic, *Virtual Shadow Maps in UE5* —
  https://dev.epicgames.com/documentation/en-us/unreal-engine/virtual-shadow-maps-in-unreal-engine
- Zhang, Lin, Wyman, Yuksel, *Shadows for many lights (full-res subset + ISM)*, EG 2025 —
  https://dqlin.xyz/pubs/2025-eg-SHA/
- Ritschel et al., *Imperfect Shadow Maps*, SIGGRAPH Asia 2008 —
  https://resources.mpi-inf.mpg.de/ImperfectShadowMaps/ISM.pdf
- Wronski, *Volumetric Fog*, SIGGRAPH 2014 — https://bartwronski.com/publications/ ·
  Hillaire, *Unified Volumetric Rendering in Frostbite*, SIGGRAPH 2015 —
  https://www.ea.com/frostbite/news/physically-based-unified-volumetric-rendering-in-frostbite
- Jansen & Bavoil, *Fourier Opacity Mapping*, I3D 2010 — https://dl.acm.org/doi/10.1145/1730804.1730831
  · Salvi et al., *Adaptive Volumetric Shadow Maps*, EGSR 2010 —
  https://onlinelibrary.wiley.com/doi/10.1111/j.1467-8659.2010.01724.x
- Epic, *MegaLights (stochastic direct lighting)*, SIGGRAPH 2025 —
  https://advances.realtimerendering.com/s2025/content/MegaLights_Stochastic_Direct_Lighting_2025.pdf
  · https://dev.epicgames.com/documentation/unreal-engine/megalights-in-unreal-engine
- Bitterli et al., *ReSTIR*, SIGGRAPH 2020 —
  https://research.nvidia.com/labs/rtr/publication/bitterli2020spatiotemporal/
- UE5 *Contact Shadows* — https://dev.epicgames.com/documentation/unreal-engine/contact-shadows-in-unreal-engine
  · Intel *XeGTAO* — https://github.com/GameTechDev/XeGTAO
- Quílez, *Soft shadows in raymarched SDFs* — https://iquilezles.org/articles/rmshadows/ ·
  Aaltonen, *GPU-based clay (Claybook SDF)*, GDC 2018 —
  https://media.gdcvault.com/gdc2018/presentations/Aaltonen_Sebastian_GPU_Based_Clay.pdf
- wgpu ray-tracing status — https://github.com/gfx-rs/wgpu/issues/6762
