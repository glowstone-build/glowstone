//! Per-pass GPU timing for the performance overlay (wgpu-29 timestamp queries).
//!
//! Each render/compute pass writes a begin+end timestamp into one shared QuerySet;
//! at frame end we `resolve_query_set` into a buffer, copy it to a mappable buffer,
//! and read it back ASYNCHRONOUSLY two frames later (never blocking the frame). The
//! per-pass tick deltas × `queue.get_timestamp_period()` give milliseconds.
//!
//! Gated on `Features::TIMESTAMP_QUERY` — absent on some drivers, so the whole thing
//! is `Option`al and the overlay degrades to a CPU-frame-ms-only HUD when missing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::shadow;

/// Number of overlay BARS (a pass-group each). Query pairs map onto these:
/// froxel = 3 compute passes summed, bloom = 3 passes summed, shadows = the dynamic
/// layer passes summed; the rest are 1:1.
pub const BARS: usize = 10;

/// Human labels + one-line sublabels for the overlay, indexed like [`PassTimings::passes`].
pub const BAR_LABELS: [(&str, &str); BARS] = [
    ("Shadows", "hero depth maps"),
    ("Depth pre", "opaque z"),
    ("Forward", "lit geometry + light loop"),
    ("SSAO", "ambient occlusion"),
    ("Froxel", "fog grid (off by default)"),
    ("Volumetric", "beam raymarch"),
    ("Vol temporal", "EMA accumulate"),
    ("Composite", "upsample + despeckle"),
    ("Bloom", "bright + blur"),
    ("Tonemap", "HDR to LDR"),
];

// Bar indices.
pub const BAR_SHADOWS: usize = 0;
pub const BAR_DEPTH: usize = 1;
pub const BAR_FORWARD: usize = 2;
pub const BAR_SSAO: usize = 3;
pub const BAR_FROXEL: usize = 4;
pub const BAR_VOL: usize = 5;
pub const BAR_VOLTEMP: usize = 6;
pub const BAR_COMPOSITE: usize = 7;
pub const BAR_BLOOM: usize = 8;
pub const BAR_TONEMAP: usize = 9;

// Query-PAIR indices (each pair = a begin/end timestamp = 2 query slots).
// Fixed passes first, then the shadow layers occupy a contiguous range so the
// dynamic 1..LAYERS count is summed into the single Shadows bar.
pub const P_DEPTH: u32 = 0;
pub const P_FORWARD: u32 = 1;
pub const P_SSAO: u32 = 2;
pub const P_FROX_INJ: u32 = 3;
pub const P_FROX_INT: u32 = 4;
pub const P_FROX_COMP: u32 = 5;
pub const P_VOL: u32 = 6;
pub const P_VOLTEMP: u32 = 7;
pub const P_COMPOSITE: u32 = 8;
pub const P_BLOOM_BRIGHT: u32 = 9;
pub const P_BLOOM_H: u32 = 10;
pub const P_BLOOM_V: u32 = 11;
pub const P_TONEMAP: u32 = 12;
pub const P_SHADOW_BASE: u32 = 13;
const PAIRS: u32 = P_SHADOW_BASE + shadow::LAYERS as u32;
const SLOTS: u32 = PAIRS * 2;
const RING: usize = 3;

/// Per-pass GPU timings (ms) + the scene "what's being rendered" counts, handed to
/// the perf-overlay UI each frame. `gpu_valid` is false when timestamps are
/// unsupported (overlay then shows only the CPU frame-ms header + the counts).
#[derive(Clone, Copy, Debug)]
pub struct PassTimings {
    pub passes: [f32; BARS],
    pub frame_ms: f32, // CPU EMA frame time (set by the app, not GPU-derived)
    pub gpu_valid: bool,
    pub fixtures: u32,
    pub beams: u32,
    pub shadow_maps: u32,
    pub geom_draws: u32,
    pub render_px: (u32, u32),
}

impl Default for PassTimings {
    fn default() -> Self {
        Self {
            passes: [0.0; BARS],
            frame_ms: 0.0,
            gpu_valid: false,
            fixtures: 0,
            beams: 0,
            shadow_maps: 0,
            geom_draws: 0,
            render_px: (0, 0),
        }
    }
}

/// QuerySet + a 3-deep ring of resolve/readback buffers + async-map state.
pub struct GpuTimers {
    query_set: wgpu::QuerySet,
    resolve: [wgpu::Buffer; RING],
    readback: [wgpu::Buffer; RING],
    /// map_async in flight on this slot (don't write or re-map it until read).
    inflight: [bool; RING],
    /// set by the map_async callback when the slot's data is readable.
    ready: [Arc<AtomicBool>; RING],
    frame: u64,
    /// Latest resolved per-pass ms (the 10 bars), persisted across stale frames.
    pub bars: [f32; BARS],
}

impl GpuTimers {
    pub fn new(device: &wgpu::Device) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("perf-timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: SLOTS,
        });
        let mk = |usage, label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: SLOTS as u64 * 8,
                usage,
                mapped_at_creation: false,
            })
        };
        let resolve = std::array::from_fn(|_| {
            mk(
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                "ts-resolve",
            )
        });
        let readback = std::array::from_fn(|_| {
            mk(
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                "ts-readback",
            )
        });
        Self {
            query_set,
            resolve,
            readback,
            inflight: [false; RING],
            ready: std::array::from_fn(|_| Arc::new(AtomicBool::new(false))),
            frame: 0,
            bars: [0.0; BARS],
        }
    }

    fn write_slot(&self) -> usize {
        (self.frame % RING as u64) as usize
    }

    /// `timestamp_writes` for a render pass timing query `pair` (begin/end), or None
    /// if that slot is busy (skip timing this frame — never corrupt a mapped buffer).
    pub fn rp(&self, pair: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        if self.inflight[self.write_slot()] {
            return None;
        }
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(pair * 2),
            end_of_pass_write_index: Some(pair * 2 + 1),
        })
    }

    /// `timestamp_writes` for a compute pass (twin of [`rp`]).
    pub fn cp(&self, pair: u32) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        if self.inflight[self.write_slot()] {
            return None;
        }
        Some(wgpu::ComputePassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(pair * 2),
            end_of_pass_write_index: Some(pair * 2 + 1),
        })
    }

    /// Append the resolve + copy to this frame's encoder (call at the end of
    /// record_scene). Skips if the write slot is still mapped from an earlier read.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let w = self.write_slot();
        if self.inflight[w] {
            return;
        }
        encoder.resolve_query_set(&self.query_set, 0..SLOTS, &self.resolve[w], 0);
        encoder.copy_buffer_to_buffer(&self.resolve[w], 0, &self.readback[w], 0, SLOTS as u64 * 8);
    }

    /// After submit: read any slot whose callback has fired, then map the slot written
    /// two frames ago (its GPU work is done by now). Never blocks. Returns true if the
    /// `bars` were refreshed this frame.
    pub fn pump(&mut self, period: f32) -> bool {
        let mut refreshed = false;
        for s in 0..RING {
            if self.ready[s].swap(false, Ordering::Acquire) {
                {
                    let view = self.readback[s].slice(..).get_mapped_range();
                    let ticks: &[u64] = bytemuck::cast_slice(&view);
                    self.bars = decode(ticks, period);
                }
                self.readback[s].unmap();
                self.inflight[s] = false;
                refreshed = true;
            }
        }
        // Map the slot written 2 frames ago (== (frame+1)%RING): its submit is done.
        if self.frame >= 2 {
            let old = ((self.frame + 1) % RING as u64) as usize;
            if !self.inflight[old] {
                self.inflight[old] = true;
                let flag = self.ready[old].clone();
                self.readback[old]
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        if r.is_ok() {
                            flag.store(true, Ordering::Release);
                        }
                    });
            }
        }
        self.frame += 1;
        refreshed
    }
}

/// Decode resolved ticks → the 10 overlay bars (ms), grouping the multi-pass bars.
fn decode(ticks: &[u64], period: f32) -> [f32; BARS] {
    let ms = |pair: u32| -> f32 {
        let (b, e) = (pair as usize * 2, pair as usize * 2 + 1);
        if e >= ticks.len() {
            return 0.0;
        }
        // Timestamps are monotonic per-queue but can wrap; clamp negatives to 0.
        let d = ticks[e].wrapping_sub(ticks[b]);
        (d as f64 * period as f64 * 1e-6) as f32 // ns → ms
    };
    let mut bars = [0.0f32; BARS];
    bars[BAR_DEPTH] = ms(P_DEPTH);
    bars[BAR_FORWARD] = ms(P_FORWARD);
    bars[BAR_SSAO] = ms(P_SSAO);
    bars[BAR_FROXEL] = ms(P_FROX_INJ) + ms(P_FROX_INT) + ms(P_FROX_COMP);
    bars[BAR_VOL] = ms(P_VOL);
    bars[BAR_VOLTEMP] = ms(P_VOLTEMP);
    bars[BAR_COMPOSITE] = ms(P_COMPOSITE);
    bars[BAR_BLOOM] = ms(P_BLOOM_BRIGHT) + ms(P_BLOOM_H) + ms(P_BLOOM_V);
    bars[BAR_TONEMAP] = ms(P_TONEMAP);
    let mut shadow = 0.0;
    for l in 0..shadow::LAYERS as u32 {
        shadow += ms(P_SHADOW_BASE + l);
    }
    bars[BAR_SHADOWS] = shadow;
    // Guard against absurd values from a wrapped/garbage frame.
    for b in bars.iter_mut() {
        if !b.is_finite() || *b > 1000.0 {
            *b = 0.0;
        }
    }
    bars
}
