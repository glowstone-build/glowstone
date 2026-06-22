export const meta = {
  name: 'volumetric-focused-pass',
  description: 'Diagnose the 3 volumetric rendering bugs and produce an ordered implementation plan for the focused fix',
  phases: [
    { title: 'Diagnose', detail: 'parallel code-traces of the 3 bugs + froxel feasibility' },
    { title: 'Plan', detail: 'synthesize one ordered, shippable implementation plan' },
  ],
}

const REPO = '/Users/aidas/conductor/workspaces/archie/carthage'

const GROUND = [
  'Project ' + REPO + ': Rust + wgpu 29 lighting previz. The volumetric beam renderer has THREE',
  'user-reported rendering bugs (incremental tweaking made it worse; the user wants a correct',
  'focused pass, NOT band-aids):',
  '',
  '1. LENS-DISCONNECT: the volumetric beam shaft does NOT connect to the fixture lens — a dark GAP',
  '   between the bright lens face and where the beam becomes visible. User insists this is a',
  '   RENDERING bug, NOT the fog-box extent.',
  '2. BANDING / "weird rendering": in a busy scene (big fog box, ~32 lit beams) the fog shows coarse',
  '   stepped diagonal bands, looks low-quality / "horrible on the eyes".',
  '3. BEAMS-THROUGH-OBJECTS: a beam passes through solid objects without occlusion (no shadow in the',
  '   shaft behind the object), INCONSISTENTLY — some beams occlude, most do not.',
  '',
  'KEY CODE FACTS (verify by reading):',
  '- Volumetric raymarch: src/shaders/volumetric.wgsl (fullscreen, HALF-RES, 64-176 adaptive steps).',
  '  fs_volumetric: reconstruct ray, clip to fog-box AABB (t_near/t_far) AND opaque depth (t_surface',
  '  = nearest depth_tex over the 2x2 footprint), march front-to-back; inner loop over ALL beams',
  '  (storage fixtures: array of Fixture); per beam: depth/range cull, radial super-Gaussian, optional',
  '  cookie, atten 1/(1+depth*depth*0.015), HG phase, and vis = opt_shadow(...) occlusion. Ray start',
  '  offset is MIDPOINT (deterministic), no per-pixel jitter (u.counts.w toggles).',
  '- Composite: src/shaders/post.wgsl fs_composite — bilinear light 5-tap of the half-res vol into',
  '  HDR (blend One,SrcAlpha = scatter + scene*transmittance).',
  '- Beam build + dispatch: src/renderer/mod.rs build_beam_gpus + record_scene (~line 1230-1370:',
  '  hero-shadow selection; adaptive step_cap = (settings.steps.max(64)*6/nbeams).clamp(64,176),',
  '  target_dt = fog_box_diagonal / step_cap).',
  '- Shadows: src/renderer/shadow.rs — ONLY the 8 sharpest lit beams (shadow::MAX=8) get a depth map',
  '  (Depth32Float 768, perspective from lens, near=(range*0.03).clamp(0.4,3), far=range). opt_shadow',
  '  in src/shaders/optics.wgsl samples it (returns 1.0/LIT outside the map). Non-hero beams set',
  '  misc.w = -1 and are NEVER occluded so they leak. Bumping MAX to 16 ~halved FOH fps (each hero is',
  '  a full geometry depth pass).',
  '- FixtureGpu lanes (src/renderer/mod.rs ~line 55): pos_range, dir_cos(w=tan_half), color(w=lens_r),',
  '  cookie_r/u, extra(x=anim,y=scroll,z=shutter/plain,w=shutter/whiteness), shape, misc(w=shadow',
  '  layer), cmyf. Lens face billboards: src/shaders/lens.wgsl.',
  '- Research (verified on the target Apple M5 Pro): docs/RESEARCH-volumetric-scaling.md (READ IT) +',
  '  docs/RESEARCH-volumetrics.md. A FROXEL grid (inject then integrate compute, 3D rgba16float,',
  '  exp-Z, trilinear lookup; Wronski/Hillaire; diharaw/volumetric-fog) + clustered light culling is',
  '  buildable here (rgba16float read-write storage works, no ping-pong; enable',
  '  TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES). Device creation: src/renderer/mod.rs Renderer::new.',
  '- Bench: PREVIZ_OPEN=show.archie, PREVIZ_BENCH=N (render-only), PREVIZ_RES, PREVIZ_CAM_*, PREVIZ_FOG,',
  '  PREVIZ_NOSHADOW. Test show ~/Downloads/performance_tuning.archie (168 fixtures, 5932 unique static',
  '  meshes, ~32 lit beams).',
].join('\n')

phase('Diagnose')
const DIAG_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['area', 'root_cause', 'evidence', 'fix_options'],
  properties: {
    area: { type: 'string' },
    root_cause: { type: 'string', description: 'precise mechanism with file:line citations' },
    evidence: { type: 'array', items: { type: 'string' } },
    fix_options: { type: 'array', items: { type: 'string' }, description: 'concrete fixes cheapest-first: file/function + expected effect + risk' },
  },
}
const tasks = [
  { k: 'lens', p: 'DIAGNOSE BUG 1 (LENS-DISCONNECT). Trace EXACTLY why the shaft does not reach the lens. Read volumetric.wgsl (does the shaft render between the lens and the fog-box entry t_near? is depth=dot(rel,bdir) culled for near samples? does t_surface clip the shaft at the EMITTING fixture own model body drawn in the mesh pass at the lens?), lens.wgsl (does the lens face write/test depth? drawn before/after the volumetric? could it occlude the shaft start?), and the lens-face vs beam-origin in build_beam_gpus. The user says it is NOT the fog box — find the real cause + concrete fixes (start shaft AT lens regardless of box entry, exclude emitter own body from near depth-clip, bridge lens-face to shaft).' },
  { k: 'banding', p: 'DIAGNOSE BUG 2 (BANDING). Trace why a big fog box + ~32 beams gives coarse stepped bands. Read the adaptive step_cap + target_dt math in record_scene and the march in volumetric.wgsl. target_dt = box_diagonal/step_cap, so a HUGE box gives large dt and coarse longitudinal sampling (midpoint = coherent bands). Concrete fixes cheapest-first: clamp target_dt to an absolute max (e.g. 0.35 m) so per-metre density is bounded regardless of box size (discuss how step_cap then bounds covered length), tighten the marched segment to the union of beam extents, importance-sample steps toward beams, or froxel exp-Z. Give visual + perf effect for each.' },
  { k: 'occlusion', p: 'DIAGNOSE BUG 3 (BEAMS-THROUGH-OBJECTS). Confirm only 8 beams get shadow maps (shadow.rs MAX + hero selection) so others set misc.w=-1 and never call opt_shadow. Read the hero selection + how misc.w gates opt_shadow in volumetric.wgsl + mesh.wgsl. Evaluate the CHEAPEST way to make ALL beams occlude WITHOUT a per-beam depth pass each (16 halved fps): (a) froxel grid with occlusion injected once per cell, (b) reuse the camera depth buffer for screen-space participating-media occlusion (note: cannot occlude light from behind a visible object), (c) one shared low-res shadow volume, (d) smarter hero selection. For each: does it actually fix the user case + cost.' },
  { k: 'froxel', p: 'DIAGNOSE FROXEL FEASIBILITY in THIS codebase. Read docs/RESEARCH-volumetric-scaling.md + docs/RESEARCH-volumetrics.md, Renderer::new device creation (features), the volumetric bind groups/pipeline in src/renderer/pipeline.rs, the FixtureGpu storage buffer, and the composite. Produce the concrete slot-in plan: device feature to enable, the 3D froxel texture(s) + format, two compute pipelines (inject loops beams incl opt_shadow + writes scatter/extinction; integrate marches +Z), exp-Z froxel-to-world mapping, composite change to a trilinear lookup. What is REUSED (optics.wgsl, FixtureGpu, fog uniforms) and the MINIMUM viable first increment (froxel for dim/wide masses + keep the existing raymarch as hero pass) vs full. Effort + risk + pitfalls (3D texture limits, workgroup sizes, bind-group churn).' },
]
const diags = (await parallel(tasks.map(t => () =>
  agent(t.p + '\n\n--- CONTEXT ---\n' + GROUND, { label: 'diag:' + t.k, phase: 'Diagnose', schema: DIAG_SCHEMA, agentType: 'Explore' })
))).filter(Boolean)

const digest = diags.map(d => '### ' + d.area + '\nROOT CAUSE: ' + d.root_cause + '\nEVIDENCE:\n- ' + d.evidence.join('\n- ') + '\nFIX OPTIONS:\n- ' + d.fix_options.join('\n- ')).join('\n\n')
log('Diagnosed ' + diags.length + ' areas')

phase('Plan')
const PLAN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['summary', 'increments', 'lens_fix', 'banding_fix', 'occlusion_fix', 'risks', 'verification'],
  properties: {
    summary: { type: 'string' },
    increments: { type: 'array', items: { type: 'object', additionalProperties: false, required: ['title', 'effort', 'steps', 'payoff'], properties: { title: { type: 'string' }, effort: { type: 'string', enum: ['S', 'M', 'L', 'XL'] }, steps: { type: 'array', items: { type: 'string' } }, payoff: { type: 'string' } } } },
    lens_fix: { type: 'string', description: 'exact change (file/function + code) for the lens-disconnect' },
    banding_fix: { type: 'string', description: 'exact change (file/function + code) for banding' },
    occlusion_fix: { type: 'string', description: 'chosen approach for beams-through-objects + why' },
    risks: { type: 'array', items: { type: 'string' } },
    verification: { type: 'array', items: { type: 'string' } },
  },
}
const plan = await agent(
  'You are the lead graphics engineer. Synthesize ONE concrete, ordered implementation plan for the focused volumetric pass fixing all three bugs WITHOUT damaging visuals, keeping FOH usable (30+ fps). Order by ROI: cheapest high-confidence fixes FIRST (lens-disconnect + banding are likely targeted shader/dispatch changes — ship immediately), then occlusion/froxel as a clearly-scoped increment. Give EXACT code for lens + banding (file + function + change). For beams-through-objects pick ONE approach and justify against cost (more hero passes is NOT acceptable). Each increment independently shippable + headlessly verifiable.\n\n--- CONTEXT ---\n' + GROUND + '\n\n--- DIAGNOSES ---\n' + digest,
  { label: 'synthesize-plan', phase: 'Plan', schema: PLAN_SCHEMA, effort: 'high' })

return plan
