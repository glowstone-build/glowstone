# Spectre — Credits & References

**Spectre** is glowstone's custom real-time rendering engine, built on [wgpu](https://wgpu.rs/). It is a forward rasteriser augmented with a volumetric raymarch and a clustered froxel-fog volume, temporal accumulation, bright-pass bloom, a filmic tonemap, and a physically-motivated fixture-optics and colour pipeline — tuned for previsualising stage lighting, moving heads, lasers, LED walls and pyro.

This file credits the research, conference talks and standards that Spectre's techniques build on. It is a thank-you and a paper trail, not a claim that any algorithm here is novel.

- **`[cited in code]`** marks a source referenced directly in the engine's source or its `docs/RESEARCH-*.md` notes.
- Entries without that marker are the canonical reference for a technique Spectre uses, even where the code did not name a source.
- Where one source is used for several things, it is listed once with all its uses gathered under it.
- Where the engine's own notes mis-attributed a source, the corrected reference is given with a short parenthetical.

Compiled by surveying the codebase (`src/shaders/*.wgsl`, `src/renderer/*`, `src/optics/*`, `docs/RESEARCH-*.md`) and web-verifying each reference. Corrections and additions are welcome.

---

## Volumetrics (fog, beams & participating media)

- **Bartłomiej Wronski (2014)** — *Volumetric Fog: Unified Compute Shader-Based Solution to Atmospheric Scattering.* SIGGRAPH 2014, Advances in Real-Time Rendering in Games. [link](https://bartwronski.com/wp-content/uploads/2014/08/bwronski_volumetric_fog_siggraph2014.pdf)  [cited in code]
  _Used in Spectre for: the frustum-aligned froxel fog volume, exponential froxel Z-slice distribution, fog self-shadowing, and the hybrid froxel + hero-beam composite._
- **Sébastien Hillaire (2015)** — *Physically Based and Unified Volumetric Rendering in Frostbite.* SIGGRAPH 2015, Advances in Real-Time Rendering in Games. [link](https://advances.realtimerendering.com/s2015/index.html)  [cited in code]
  _Used in Spectre for: the energy-conserving analytic per-slice scattering integral and unified froxel architecture._
- **Nelson Max (1986)** — *Atmospheric Illumination and Shadows.* Computer Graphics (Proc. SIGGRAPH '86), 20(4), 117–124. [link](https://dl.acm.org/doi/10.1145/15886.15899)  [cited in code]
  _Used in Spectre for: single-scattering volumetric raymarch along view rays._
- **Louis G. Henyey & Jesse L. Greenstein (1941)** — *Diffuse Radiation in the Galaxy.* Astrophysical Journal, 93, 70–83. [link](https://ui.adsabs.harvard.edu/abs/1941ApJ....93...70H/abstract)  [cited in code]
  _Used in Spectre for: the exact Henyey–Greenstein scattering phase function (forward-peak convention) in the beam and froxel passes._
- **Philippe Blasi, Bertrand Le Saec & Christophe Schlick (1993)** — *A Rendering Algorithm for Discrete Volume Density Objects.* Computer Graphics Forum 12(3) (Eurographics '93), C201–C210. [link](https://onlinelibrary.wiley.com/doi/abs/10.1111/1467-8659.1230201)  [cited in code]
  _Used in Spectre for: the Schlick cheap-phase approximation, compared against exact HG (the code cites a Shadertoy demo, which is an illustration rather than the origin)._
- **William M. Cornette & Joseph G. Shanks (1992)** — *Physically Reasonable Analytic Expression for the Single-Scattering Phase Function.* Applied Optics 31(16), 3152–3160. [link](https://opg.optica.org/ao/abstract.cfm?uri=ao-31-16-3152)
  _Used in Spectre for: the Cornette–Shanks phase-function variant._
- **Bo Sun, Ravi Ramamoorthi, Srinivasa G. Narasimhan & Shree K. Nayar (2005)** — *A Practical Analytic Single Scattering Model for Real Time Rendering.* ACM TOG 24(3) (SIGGRAPH 2005), 1040–1049. [link](https://dl.acm.org/doi/10.1145/1186822.1073309)  [cited in code]
  _Used in Spectre for: the analytic single-scattering airlight model._
- **Ola Olsson, Markus Billeter & Ulf Assarsson (2012)** — *Clustered Deferred and Forward Shading.* HPG 2012, 87–96. [link](https://www.cse.chalmers.se/~uffe/clustered_shading_preprint.pdf)  [cited in code]
  _Used in Spectre for: clustered/froxel depth-slice shading foundations._
- **Ola Olsson & Ulf Assarsson (2011)** — *Tiled Shading.* Journal of Graphics, GPU, and Game Tools 15(4), 235–251. [link](https://doi.org/10.1080/2151237X.2011.621761)
  _Used in Spectre for: per-screen-tile light-list culling (CSR light lists) for the volumetric and mesh passes._
- **Takahiro Harada, Jay McKee & Jason C. Yang (2012)** — *Forward+: A Step Toward Film-Style Shading in Real Time.* GPU Pro 3 (CRC Press); Eurographics 2012. [link](https://www.taylorfrancis.com/chapters/edit/10.1201/9781351261524-18/forward-step-toward-film-style-shading-real-time-takahiro-harada-jay-mckee-jason-yang)  [cited in code]
  _Used in Spectre for: tiled (Forward+) light culling per screen tile._
- **Tiago Sousa & Jean Geffroy (2016)** — *The Devil is in the Details: idTech 666 (DOOM).* SIGGRAPH 2016, Advances in Real-Time Rendering (popularly documented in Adrian Courrèges' DOOM Graphics Study). [link](https://www.adriancourreges.com/blog/2016/09/09/doom-2016-graphics-study/)  [cited in code]
  _Used in Spectre for: the exponential/logarithmic view-Z slice distribution (also seen in Filament's Froxelizer)._
- **Harald Koschmieder (1924)** — *Theorie der horizontalen Sichtweite.* Beiträge zur Physik der freien Atmosphäre 12, 33–53. [link](https://opg.optica.org/ao/abstract.cfm?uri=ao-44-18-3795)  [cited in code]
  _Used in Spectre for: the extinction-coefficient ↔ visibility relationship (σ = 3.912/V). The code's note cites "Kim et al." at that URL, which actually resolves to Nebuloni (2005), "Empirical relationships between extinction coefficient and visibility in fog," Applied Optics 44(18) — the "Kim et al." attribution is a mis-citation._

_Spectre also uses standard volumetric techniques with no single seminal source: constant-DT step counts and the Beer–Lambert emission–absorption march (descended from Kajiya & Von Herzen, "Ray Tracing Volume Densities," SIGGRAPH 1984), plus the engine-specific hybrid froxel + hero-beam composite architecture._

### Researched but not (yet) shipped — volumetric scaling & many-lights

Surveyed in `docs/RESEARCH-volumetric-scaling.md` / `docs/RESEARCH-shadows.md` as scaling directions, and credited here as the source material consulted:

- **Benedikt Bitterli, Chris Wyman, Matt Pharr, Peter Shirley, Aaron Lefohn & Wojciech Jarosz (2020)** — *Spatiotemporal Reservoir Resampling for Real-Time Ray Tracing with Dynamic Direct Lighting (ReSTIR).* ACM TOG 39(4) (SIGGRAPH 2020). [link](https://doi.org/10.1145/3386569.3392481)  [cited in code]
  _Used in Spectre for: spatio-temporal importance-resampling research (the code's note cites "Lin et al., SIGGRAPH Asia 2021," which is the later GRIS/ReSTIR-PT generalisation, not the origin)._
- **Alejandro Conty Estevez & Christopher Kulla (2018)** — *Importance Sampling of Many Lights with Adaptive Tree Splitting.* PACMCGIT 1(2) (HPG 2018). [link](https://doi.org/10.1145/3233305)  [cited in code]
  _Used in Spectre for: light-tree importance-sampling research._
- **Yuriy O'Donnell & Matthäus G. Chajdas (2017)** — *Tiled Light Trees.* I3D 2017. [link](https://dl.acm.org/doi/10.1145/3023368.3023376)  [cited in code]
  _Used in Spectre for: the hybrid tiled-light-tree structure research (the code's note attributes this to "Yakovlev et al."; the actual authors are O'Donnell (EA) & Chajdas (AMD))._

---

## Tone mapping, bloom & post-processing

- **Krzysztof Narkowicz (2016)** — *ACES Filmic Tone Mapping Curve.* Blog (knarkowicz.wordpress.com). [link](https://knarkowicz.wordpress.com/2016/01/06/aces-filmic-tone-mapping-curve/)  [cited in code]
  _Used in Spectre for: the fitted-rational ACES filmic tonemap operator._
- **Academy of Motion Picture Arts and Sciences (AMPAS)** — *Academy Color Encoding System (ACES) — RRT + ODT reference transforms.* aces-dev. [link](https://github.com/ampas/aces-dev)
  _Used in Spectre for: the ACES tone-reproduction operator that Narkowicz's curve approximates._
- **Greg James & John O'Rorke (2004)** — *Real-Time Glow.* GPU Gems, Ch. 21 (NVIDIA / Addison-Wesley). [link](https://developer.nvidia.com/gpugems/gpugems/part-iv-image-processing/chapter-21-real-time-glow)
  _Used in Spectre for: the bright-pass bloom pipeline (extraction → separable Gaussian blur → add)._
- **Jorge Jiménez (2014)** — *Next Generation Post Processing in Call of Duty: Advanced Warfare.* SIGGRAPH 2014, Advances in Real-Time Rendering in Games. [link](https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/)  [cited in code]
  _Used in Spectre for: interleaved gradient noise (per-pixel jitter / anti-banding) and the partial-Karis-average anti-firefly bloom downsample weighting._
- **Johannes Kopf, Michael F. Cohen, Dani Lischinski & Matt Uyttendaele (2007)** — *Joint Bilateral Upsampling.* ACM TOG 26(3) (SIGGRAPH 2007). [link](https://doi.org/10.1145/1276377.1276497)  [cited in code]
  _Used in Spectre for: the depth-aware joint-bilateral upsample of the low-res volumetric buffer into the full-res frame._
- **Angelo Pesce (c0de517e) (2016)** — *Low-Resolution Effects with Depth-Aware Upsampling* (as used for Black Ops 3 volumetrics). [link](http://c0de517e.blogspot.com/2016/02/downsampled-effects-with-depth-aware.html)  [cited in code]
  _Used in Spectre for: the nearest-depth practical variant of the bilateral upsample for the volumetric composite._
- **Kenny Mitchell (2007)** — *Volumetric Light Scattering as a Post-Process.* GPU Gems 3, Ch. 13 (NVIDIA / Addison-Wesley). [link](https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-13-volumetric-light-scattering-post-process)  [cited in code]
  _Used in Spectre for: depth-aware/bilateral upsampling reference for volumetric scattering (the code's "Gem 3" note credits Jason Mitchell; the chapter author is Kenny Mitchell of EA)._
- **Carlo Tomasi & Roberto Manduchi (1998)** — *Bilateral Filtering for Gray and Color Images.* Proc. IEEE ICCV, 839–846. [link](https://ieeexplore.ieee.org/document/710815)
  _Used in Spectre for: the edge-preserving bilateral filter underlying depth-aware despeckle and upsampling._
- **Martin Mittring (2007)** — *Finding Next Gen — CryEngine 2.* SIGGRAPH 2007, Advanced Real-Time Rendering in 3D Graphics and Games. [link](https://artis.inrialpes.fr/Membres/Olivier.Hoel/ssao/p97-mittring.pdf)
  _Used in Spectre for: depth-only screen-space ambient occlusion (SSAO)._
- **Erik Reinhard, Michael Stark, Peter Shirley & James Ferwerda (2002)** — *Photographic Tone Reproduction for Digital Images.* ACM TOG 21(3) (SIGGRAPH 2002), 267–276. [link](https://dl.acm.org/doi/10.1145/566570.566575)
  _Used in Spectre for: HDR bright-value desaturation toward white (the "HDR whitening boost" for bright pixel cells)._

_Spectre also uses standard image-processing techniques with no single seminal source: the separability of the 2D Gaussian into two 1D passes, contour-preserving unsharp-mask edge steepening (a digital descendant of darkroom unsharp masking, Spiegler & Juris 1931), and sRGB output gamma (credited below under Colour science)._

---

## Temporal accumulation & anti-aliasing

- **Diego Nehab, Pedro V. Sander, Jason Lawrence, Natalya Tatarchuk & John Isidoro (2007)** — *Accelerating Real-Time Shading with Reverse Reprojection Caching.* Graphics Hardware 2007. [link](https://dl.acm.org/doi/10.5555/1280094.1280098)
  _Used in Spectre for: temporal reprojection of the history buffer (the EMA / exponential-feedback blend itself is generic TAA practice)._
- **Bartłomiej Wronski (2014)** — *Volumetric Fog: Unified Compute Shader-Based Solution to Atmospheric Scattering.* SIGGRAPH 2014. [link](https://bartwronski.files.wordpress.com/2014/08/bwronski_volumetric_fog_siggraph2014.pdf)  [cited in code]
  _Used in Spectre for: temporal reprojection + exponential-moving-average accumulation in the froxel volume to denoise volumetric integration._
- **Lance Williams (1978)** — *Casting Curved Shadows on Curved Surfaces.* Proc. SIGGRAPH '78, 270–274. [link](https://cseweb.ucsd.edu/~ravir/274/15/papers/p270-williams.pdf)
  _Used in Spectre for: shadow mapping and the depth-bias offset (also under Shadows); the depth-only pre-pass + EQUAL-test main pass for overdraw reduction is established console-rendering folklore with no single seminal paper._

_The spatiotemporal blue-noise sample offsets that drive the volumetric temporal resolve are credited under Sampling, noise & dithering._

---

## Sampling, noise & dithering

- **Jorge Jiménez (2014)** — *Next Generation Post Processing in Call of Duty: Advanced Warfare.* SIGGRAPH 2014, Advances in Real-Time Rendering in Games. [link](https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/)  [cited in code]
  _Used in Spectre for: interleaved gradient noise (IGN) blue-noise dither / ray jitter (the code's notes cite Demofox's explainer; Jiménez is the seminal source)._
- **Robert A. Ulichney (1993)** — *The Void-and-Cluster Method for Dither Array Generation.* Proc. SPIE 1913, 332–343. [link](https://cv.ulichney.com/papers/1993-void-cluster.pdf)  [cited in code]
  _Used in Spectre for: blue-noise mask generation underlying the ray-march dithering._
- **Alan Wolfe, Nathan Morrical, Tomas Akenine-Möller & Ravi Ramamoorthi (2022)** — *Spatiotemporal Blue Noise Masks.* EGSR 2022 (arXiv:2112.09629; UE `BlueNoise.ush`). [link](https://research.nvidia.com/publication/2022-07_spatiotemporal-blue-noise-masks)  [cited in code]
  _Used in Spectre for: animated spatiotemporal blue-noise (STBN) sample offsets for the volumetric march and temporal resolve._
- **Alan Wolfe (Demofox) (2020)** — *Ray Marching Fog With Blue Noise.* blog.demofox.org. [link](https://blog.demofox.org/2020/05/10/ray-marching-fog-with-blue-noise/)  [cited in code]
  _Used in Spectre for: the cited explainer for blue-noise dithering of the fog ray-march._
- **Ken Perlin (1985)** — *An Image Synthesizer.* Computer Graphics (SIGGRAPH '85) 19(3), 287–296. [link](https://dl.acm.org/doi/10.1145/325165.325247)
  _Used in Spectre for: Perlin/value noise and the octave-summed fractal-sum (fBm) construction for haze density and 3D noise generation._
- **Ken Perlin (2002)** — *Improving Noise.* ACM TOG 21(3) (SIGGRAPH 2002), 681–682. [link](https://mrl.cs.nyu.edu/~perlin/paper445.pdf)
  _Used in Spectre for: the quintic fade curve (6t⁵−15t⁴+10t³) in value-noise interpolation._
- **Benoit B. Mandelbrot & John W. van Ness (1968)** — *Fractional Brownian Motions, Fractional Noises and Applications.* SIAM Review 10(4), 422–437. [link](https://iquilezles.org/articles/fbm/)
  _Used in Spectre for: the fractional-Brownian-motion (fBm) basis; the shader formulation follows Iñigo Quílez's fBm articles._
- **Iñigo Quílez** — *Domain Warping.* iquilezles.org. [link](https://iquilezles.org/articles/warp/)  [cited in code]
  _Used in Spectre for: domain-warped noise (CO₂ "cauliflower" detail and procedural flow)._
- **Helmut Vogel (1979)** — *A Better Way to Construct the Sunflower Head.* Mathematical Biosciences 44(3–4), 179–189. [link](https://doi.org/10.1016/0025-5564(79)90080-4)
  _Used in Spectre for: the golden-angle (Vogel/Fermat) spiral disc-sampling pattern used in SSAO/PCF kernels._

---

## Shadows & ambient occlusion

- **William T. Reeves, David H. Salesin & Robert L. Cook (1987)** — *Rendering Antialiased Shadows with Depth Maps.* Computer Graphics 21(4) (SIGGRAPH '87), 283–291. [link](https://graphics.pixar.com/library/ShadowMaps/paper.pdf)
  _Used in Spectre for: Percentage-Closer Filtering (PCF) hard shadows, realised via the 2×2 hardware comparison sampler._
- **Lance Williams (1978)** — *Casting Curved Shadows on Curved Surfaces.* Proc. SIGGRAPH '78, 270–274. [link](https://cseweb.ucsd.edu/~ravir/274/15/papers/p270-williams.pdf)
  _Used in Spectre for: shadow mapping and the depth-bias offset against self-shadowing (the slope-scale refinement is later hardware folklore)._
- **Martin Mittring (2007)** — *Finding Next Gen — CryEngine 2.* SIGGRAPH 2007, Advanced Real-Time Rendering in 3D Graphics and Games, 97–121. [link](https://dl.acm.org/doi/10.1145/1281500.1281671)
  _Used in Spectre for: depth-only screen-space ambient occlusion (SSAO)._

### Researched but not (yet) shipped — many-lights shadowing

Surveyed in `docs/RESEARCH-shadows.md` as future directions:

- **Ola Olsson, Markus Billeter, Erik Sintorn, Viktor Kämpe & Ulf Assarsson (2015)** — *More Efficient Virtual Shadow Maps for Many Lights.* IEEE TVCG 21(6), 701–713. [link](https://efficientshading.com/2015/06/01/more-efficient-virtual-shadow-maps-for-many-lights/)  [cited in code]
  _Used in Spectre for: virtual-shadow-map research for many-light shadowing._
- **Tobias Ritschel, Thorsten Grosch, Min H. Kim, Hans-Peter Seidel, Carsten Dachsbacher & Jan Kautz (2008)** — *Imperfect Shadow Maps for Efficient Computation of Indirect Illumination.* ACM TOG 27(5) (SIGGRAPH Asia 2008). [link](https://resources.mpi-inf.mpg.de/ImperfectShadowMaps/)  [cited in code]
  _Used in Spectre for: imperfect-shadow-map research._
- **Jon Jansen & Louis Bavoil (2010)** — *Fourier Opacity Mapping.* I3D 2010. [link](https://dl.acm.org/doi/10.1145/1730804.1730831)  [cited in code]
  _Used in Spectre for: volumetric-shadow research (Fourier opacity maps)._
- **Marco Salvi, Kiril Vidimče, Andrew Lauritzen & Aaron Lefohn (2010)** — *Adaptive Volumetric Shadow Maps.* Computer Graphics Forum 29(4) (EGSR 2010). [link](https://doi.org/10.1111/j.1467-8659.2010.01724.x)  [cited in code]
  _Used in Spectre for: volumetric-shadow research (adaptive volumetric shadow maps)._
- **Thomas Engelhardt & Carsten Dachsbacher (2010)** — *Epipolar Sampling for Shadows and Crepuscular Rays in Participating Media with Single Scattering.* I3D 2010, 119–125. [link](https://dl.acm.org/doi/10.1145/1730804.1730823)  [cited in code]
  _Used in Spectre for: epipolar-sampling research for volumetric shadows._
- **Jiawen Chen, Ilya Baran, Frédo Durand & Wojciech Jarosz (2011)** — *Real-Time Volumetric Shadows using 1D Min-Max Mipmaps.* I3D 2011. [link](https://cs.dartmouth.edu/~wjarosz/publications/chen11realtime.html)  [cited in code]
  _Used in Spectre for: 1D min-max mipmap volumetric-shadow research._
- **Krzysztof Narkowicz & Tiago Costa, Epic Games (2025)** — *MegaLights: Stochastic Direct Lighting in Unreal Engine 5.* SIGGRAPH 2025, Advances in Real-Time Rendering. [link](https://advances.realtimerendering.com/s2025/content/MegaLights_Stochastic_Direct_Lighting_2025.pdf)  [cited in code]
  _Used in Spectre for: stochastic many-light direct-lighting research._
- **Jorge Jiménez, Xian-Chun Wu, Angelo Pesce & Adrian Jarabo (2016)** — *Practical Real-Time Strategies for Accurate Indirect Occlusion (GTAO).* SIGGRAPH 2016 (implemented as XeGTAO by Filip Strugar, Intel). [link](https://www.activision.com/cdn/research/Practical_Real_Time_Strategies_for_Accurate_Indirect_Occlusion_NEW%20VERSION_COLOR.pdf)  [cited in code]
  _Used in Spectre for: ground-truth ambient-occlusion (XeGTAO) research._
- **Epic Games** — *Contact Shadows in Unreal Engine.* Official documentation. [link](https://dev.epicgames.com/documentation/unreal-engine/contact-shadows-in-unreal-engine)  [cited in code]
  _Used in Spectre for: screen-space contact-shadow research._
- **Iñigo Quílez** — *Soft Shadows in Raymarched SDFs.* iquilezles.org. [link](https://iquilezles.org/articles/rmshadows/)  [cited in code]
  _Used in Spectre for: penumbra soft-shadow research for raymarched SDFs._
- **Sebastian Aaltonen (2018)** — *GPU-Based Clay Simulation and Ray-Tracing Tech in 'Claybook'.* GDC 2018. [link](https://media.gdcvault.com/gdc2018/presentations/Aaltonen_Sebastian_GPU_Based_Clay.pdf)  [cited in code]
  _Used in Spectre for: GPU-based SDF/clay-rendering research._

---

## Colour science

- **Mieczysław Krystek (1985)** — *An Algorithm to Calculate Correlated Colour Temperature.* Color Research & Application 10(1), 38–40. [link](https://onlinelibrary.wiley.com/doi/10.1002/col.5080100109)  [cited in code]
  _Used in Spectre for: the Planckian-locus rational fit (CCT → chromaticity, accurate 1000–15000 K)._
- **IEC 61966-2-1:1999 (Stokes, Anderson, Chandrasekar & Motta, 1996)** — *Default RGB Colour Space — sRGB.* W3C / IEC. [link](https://www.w3.org/Graphics/Color/sRGB.html)  [cited in code]
  _Used in Spectre for: sRGB output gamma correction and the linear-RGB ↔ CIE XYZ (D65) conversion matrices._
- **Hermann von Helmholtz (1867) & V. A. Kohlrausch (1923); modern model: Yoshinobu Nayatani (1997)** — *Simple Estimation Methods for the Helmholtz–Kohlrausch Effect.* Color Research & Application 22(6), 385–401. [link](https://en.wikipedia.org/wiki/Helmholtz%E2%80%93Kohlrausch_effect)
  _Used in Spectre for: chroma "read-up" — boosting the perceived brightness of saturated hues at equal luminance, in the beam and froxel passes._
- **Max Planck (1901)** — *Über das Gesetz der Energieverteilung im Normalspectrum.* Annalen der Physik 4, 553–563. [link](https://doi.org/10.1002/andp.19013090310)
  _Used in Spectre for: blackbody spectral radiance from temperature (the physical basis for CCT → colour)._
- **Tanner Helland (2012)** — *How to Convert Temperature (K) to RGB.* tannerhelland.com. [link](https://tannerhelland.com/2012/09/18/convert-temperature-rgb-algorithm-code.html)
  _Used in Spectre for: the blackbody-temperature → RGB curve-fit (cold-spark / pyro colour-over-life)._
- **Scott A. Burns (2017)** — *Subtractive Color Mixture Computation.* arXiv:1710.06364. [link](https://arxiv.org/abs/1710.06364)  [cited in code]
  _Used in Spectre for: subtractive CMY dichroic colour mixing in the optical-density (log-add / geometric-mean) domain._
- **Yoshi Ohno (2014)** — *Practical Use and Calculation of CCT and Duv.* LEUKOS 10(1), 47–55. [link](https://www.tandfonline.com/doi/abs/10.1080/15502724.2014.839020)
  _Used in Spectre for: Duv-style green/magenta tint correction (signed perpendicular distance from the Planckian locus)._
- **ITU-R BT.709** — *Parameter Values for the HDTV Standards for Production and International Programme Exchange.* [link](https://www.itu.int/rec/R-REC-BT.709)  [cited in code]
  _Used in Spectre for: Rec.709 relative-luminance weights (CCT-to-RGB normalisation) and YUV → RGB conversion for video sources._
- **Jason Mitchell, Gary McTaggart & Chris Green (2006)** — *Shading in Valve's Source Engine.* SIGGRAPH 2006, Advanced Real-Time Rendering in 3D Graphics and Games. [link](https://advances.realtimerendering.com/s2006/Chapter7-Shading_in_Valve's_Source_Engine.pdf)
  _Used in Spectre for: Half-Lambert wrap lighting on set geometry._

_Spectre also uses several colour conventions with no single seminal source: the dichroic transmittance model (spectral shoulders + convex insertion ramp) is a project-internal phenomenological formulation (`research-dichroic.md`), and the GDTF `ColorMixMSpeed` exponential time mapping is an engine convention anchored to the GDTF attribute (`research-cmy.md`)._

---

## Fixture optics & GDTF / MVR standards

- **GDTF — Vectorworks, MA Lighting & Robe Lighting (2018; DIN SPEC 15800, 2020/2022)** — *General Device Type Format.* [link](https://github.com/mvrdevelopment/spec/blob/main/gdtf-spec.md)  [cited in code]
  _Used in Spectre for: GDTF parsing; GeometryReference DMX instancing with `<Break>` rows; and simulating the physical mechanisms it standardises — rotating gobo wheels (slot interpolation), colour wheels, CMY graduated dichroic flags, animation wheels, mechanical/framing shutters, and prism facets._
- **MVR (My Virtual Rig) — Vectorworks, MA Lighting & Robe Lighting (2018; DIN SPEC 15801)** — *My Virtual Rig scene-exchange format.* [link](https://gdtf-share.com/help/en/help/mvr/index.html)  [cited in code]
  _Used in Spectre for: MVR scene import/export._
- **Michael Potmesil & Indranil Chakravarty (1981)** — *A Lens and Aperture Camera Model for Synthetic Image Generation.* Computer Graphics 15(3) (SIGGRAPH '81), 297–305. [link](https://doi.org/10.1145/965161.806818)
  _Used in Spectre for: the thin-lens / circle-of-confusion model behind aperture- and diopter-based depth of field._
- **Joe Demers (2004)** — *Depth of Field: A Survey of Techniques.* GPU Gems, Ch. 23 (NVIDIA / Addison-Wesley). [link](https://developer.nvidia.com/gpugems/gpugems/part-iv-image-processing/chapter-23-depth-field-survey-techniques)  [cited in code]
  _Used in Spectre for: the real-time circle-of-confusion defocus implementation (`research-focus-dof.md`)._
- **David R. Warn (1983)** — *Lighting Controls for Synthetic Images.* Computer Graphics 17(3) (SIGGRAPH '83), 13–21. [link](https://dl.acm.org/doi/10.1145/964967.801127)
  _Used in Spectre for: the parametric spotlight cone (concentration/falloff exponent, flaps) behind the disc-light truncated-cone beam projector._
- **Augustin-Jean Fresnel (1823)** — *Mémoire sur la loi des modifications que la réflexion imprime à la lumière polarisée* (Fresnel equations & total internal reflection); real-time form via **Christophe Schlick (1994)**, *An Inexpensive BRDF Model for Physically-Based Rendering*, CGF 13(3), 233–246. [link](https://en.wikipedia.org/wiki/Fresnel_equations)
  _Used in Spectre for: Fresnel reflection and total internal reflection (TIR) on the lens face._
- **F. Gori (1994)** — *Flattened Gaussian Beams.* Optics Communications 107(5–6), 335–341. [link](https://doi.org/10.1016/0030-4018(94)90342-5)
  _Used in Spectre for: the super-Gaussian (flat-top, tunable-order) radial beam falloff controlling spot-vs-wash edge hardness._
- **Claude E. Duchon (1979)** — *Lanczos Filtering in One and Two Dimensions.* Journal of Applied Meteorology 18(8), 1016–1022. [link](https://journals.ametsoc.org/view/journals/apme/18/8/1520-0450_1979_018_1016_lfioat_2_0_co_2.xml)
  _Used in Spectre for: Lanczos3 resampling of the gobo texture atlas (paired with unsharp-mask edge steepening)._
- **CIE (1924)** — *Photopic Luminous Efficiency Function V(λ).* [link](https://cie.co.at/datatable/cie-1924-photopic-vm-function-2-degree)  [cited in code]
  _Used in Spectre for: luminous-flux photometry (converting radiometric power to lumens for brightness normalisation)._

_Spectre also models several effects from physics/engineering convention with no single CG-seminal source: étendue / luminance conservation on the lens face (the L/n² invariant); prism facet-count expansion (an engine heuristic over the GDTF prism convention); beam-hotspot vs field-angle photometry (IES conventions); laser collimation with no inverse-square falloff (Gaussian-beam optics); and lateral chromatic aberration as a per-channel UV shift (wavelength-dependent dispersion)._

---

## Image-based lighting (HDRI)

- **Paul Debevec (1998)** — *Rendering Synthetic Objects into Real Scenes.* Proc. SIGGRAPH '98, 189–198. [link](https://www.pauldebevec.com/Research/IBL/)
  _Used in Spectre for: image-based lighting from a mipped HDRI environment (diffuse + specular environment lookups)._
- **James F. Blinn & Martin E. Newell (1976)** — *Texture and Reflection in Computer Generated Images.* CACM 19(10), 542–547 (SIGGRAPH '76). [link](https://dl.acm.org/doi/10.1145/360349.360353)
  _Used in Spectre for: the latitude-longitude (equirectangular) environment-map projection for the HDRI sky._
- **Ravi Ramamoorthi & Pat Hanrahan (2001)** — *An Efficient Representation for Irradiance Environment Maps.* Proc. SIGGRAPH 2001, 497–500. [link](https://dl.acm.org/doi/10.1145/383259.383317)
  _Used in Spectre for: low-frequency diffuse irradiance from the environment (the practical real-time form being prefiltered-mip sampling)._
- **Jan Kautz, Pere-Pau Vázquez, Wolfgang Heidrich & Hans-Peter Seidel (2000)** — *A Unified Approach to Prefiltered Environment Maps.* Eurographics Workshop on Rendering 2000. [link](https://www.semanticscholar.org/paper/Unified-Approach-to-Prefiltered-Environment-Maps-Kautz-V%C3%A1zquez/aa040971c9d2df7a897db533b3aecc494c0446a4)
  _Used in Spectre for: the box-filter mip-chain prefiltering of the HDRI for runtime IBL._
- **Eric Heitz, Jonathan Dupuy, Stephen Hill & David Neubelt (2016)** — *Real-Time Polygonal-Light Shading with Linearly Transformed Cosines.* ACM TOG 35(4) (SIGGRAPH 2016). [link](https://eheitzresearch.wordpress.com/415-2/)
  _Used in Spectre for: the analytic area-light basis behind aperture-based LED-wall area lighting._
- **Florian Kainz, Rod Bogart & Drew Hess (2003)** — *The OpenEXR Image File Format / `half` 16-bit Float.* ILM (SIGGRAPH 2003; GPU Gems Ch. 26). [link](https://developer.nvidia.com/gpugems/gpugems/part-iv-image-processing/chapter-26-openexr-image-file-format)
  _Used in Spectre for: the F16 half-precision HDR texture format (with clamp-to-half-max to avoid inf)._

---

## Particles & pyro

- **Robert Bridson, Jim Hourihan & Marcus Nordenstam (2007)** — *Curl-Noise for Procedural Fluid Flow.* ACM TOG 26(3) (SIGGRAPH 2007), Article 46. [link](https://www.cs.ubc.ca/~rbridson/docs/bridson-siggraph2007-curlnoise.pdf)  [cited in code]
  _Used in Spectre for: divergence-free curl-noise turbulence (v = curl(ψ) from uncorrelated fBm potential fields) on smoke and spark particles._
- **George Gabriel Stokes (1851)** — *On the Effect of the Internal Friction of Fluids on the Motion of Pendulums.* Trans. Cambridge Phil. Soc. 9. [link](https://ui.adsabs.harvard.edu/abs/1851TCaPS...9....8S/abstract)  [cited in code]
  _Used in Spectre for: linear (creeping-flow) Stokes drag damping in ballistic particle integration._
- **Lee Westover (1990)** — *Footprint Evaluation for Volume Rendering.* Computer Graphics 24(4) (SIGGRAPH '90), 367–376. [link](https://doi.org/10.1145/97879.97919)
  _Used in Spectre for: Gaussian-kernel core+halo particle falloff (splatting reconstruction kernels)._
- **Robert A. Gingold & Joseph J. Monaghan (1977)** — *Smoothed Particle Hydrodynamics: Theory and Application to Non-Spherical Stars.* MNRAS 181(3), 375–389. [link](https://ui.adsabs.harvard.edu/abs/1977MNRAS.181..375G/abstract)
  _Used in Spectre for: SPH-style Gaussian-kernel particle-to-grid deposition, voxelising CO₂ particles into a 3D density grid (note: this is kernel splatting, not 3D Gaussian Splatting)._
- **J. Stewart Turner (1957)** — *Buoyant Vortex Rings.* Proc. R. Soc. Lond. A 239, 61–75. [link](https://royalsocietypublishing.org/doi/10.1098/rspa.1957.0022)
  _Used in Spectre for: the vortex-ring "mushroom-cap" advection of the buoyant CO₂ starting plume._
- **Andrew Schneider & Nathan Vos (2015)** — *The Real-Time Volumetric Cloudscapes of Horizon: Zero Dawn.* SIGGRAPH 2015, Advances in Real-Time Rendering. [link](https://www.advances.realtimerendering.com/s2015/The%20Real-time%20Volumetric%20Cloudscapes%20of%20Horizon%20-%20Zero%20Dawn%20-%20ARTR.pdf)
  _Used in Spectre for: the Perlin-Worley "cauliflower" billow look and Beer-powder interior self-shadowing of CO₂ smoke._
- **William T. Reeves (1983)** — *Particle Systems — A Technique for Modeling a Class of Fuzzy Objects.* ACM TOG 2(2), 91–108. [link](https://dl.acm.org/doi/10.1145/357318.357320)
  _Used in Spectre for: the particle-system foundation behind cold-spark fountains and ballistic emitters._

_Spectre also draws on engine-documentation folklore (`docs/RESEARCH-pyro.md`): velocity-stretched billboards (UE4 Cascade / Unity stretched-billboard mode) and velocity-cone particle launch (UE `ParticleModuleVelocityCone`) — standard VFX practice with no single seminal paper._

---

## Geometry, meshes & LED surfaces

- **Francesco Carucci (2005)** — *Inside Geometry Instancing.* GPU Gems 2, Ch. 3 (NVIDIA / Addison-Wesley), 47–67. [link](https://developer.nvidia.com/gpugems/gpugems2/part-i-geometric-complexity/chapter-3-inside-geometry-instancing)
  _Used in Spectre for: hardware geometry instancing (MeshInstance, LensInstance, WallInstance, ParticleInstance)._
- **Thomas Porter & Tom Duff (1984)** — *Compositing Digital Images.* Computer Graphics 18(3) (SIGGRAPH '84), 253–259. [link](https://history.siggraph.org/learning/compositing-digital-images-by-porter-and-duff/)
  _Used in Spectre for: premultiplied/associated alpha (the OVER operator) for transparent LED walls, and the PLUS-operator limit for additive emissive sparks._
- **Takafumi Saito & Tokiichiro Takahashi (1990)** — *Comprehensible Rendering of 3-D Shapes.* Computer Graphics 24(4) (SIGGRAPH '90), 197–206. [link](https://dl.acm.org/doi/10.1145/97880.97901)
  _Used in Spectre for: G-buffer image-space edge detection behind selection-silhouette outlines._
- **The Khronos 3D Formats Working Group (2017)** — *glTF 2.0 Specification.* Khronos Group. [link](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html)
  _Used in Spectre for: glTF/GLB scene-graph traversal and TRS → matrix node-transform composition._
- **Paul Bourke** — *The 3D Studio File Format (.3ds).* paulbourke.net. [link](https://paulbourke.net/dataformats/3ds/)
  _Used in Spectre for: parsing the (never officially published) .3ds binary chunk hierarchy._
- **John Platt (2000)** — *Optimal Filtering for Patterned Displays.* IEEE Signal Processing Letters (Microsoft ClearType). [link](https://en.wikipedia.org/wiki/Subpixel_rendering)
  _Used in Spectre for: discrete RGB sub-pixel stripe rendering when LED panels are viewed up close._

_Spectre also uses several folklore conventions with no seminal source: two-sided surface shading (back-facing normal flip, codified in the OpenGL spec), perspective ↔ orthographic projection blending, the direct-view LED pixel-pitch vs viewing-distance "reveal" relationship and inter-cabinet seam overlays (LED-wall display-engineering practice), and circular-arc parametrisation for curved LED walls._

---

## General rendering & architecture

- **Robert Penner (2002)** — *Programming Macromedia Flash MX, Ch. 7: Motion, Tweening, and Easing* (the canonical easing equations). [link](https://robertpenner.com/easing/)  [cited in code]
  _Used in Spectre for: the cubic ease-out camera animation curve (matching Unreal's `ECurveEaseFunction::CubicOut`)._
- **Imagination Technologies — PowerVR Tile-Based Deferred Rendering** (popularised for Apple GPUs at WWDC 2020). [link](https://docs.imgtec.com/starter-guides/powervr-architecture/html/topics/tile-based-deferred-rendering-index.html)  [cited in code]
  _Used in Spectre for: TBDR architecture awareness in the volumetric-scaling research._
- **NewTek / Vizrt Group (2015)** — *NDI (Network Device Interface) SDK.* [link](https://ndi.video/sdk/)  [cited in code]
  _Used in Spectre for: NDI video receive/codecs research for LED-wall sources (`docs/RESEARCH-led-ndi.md`)._
