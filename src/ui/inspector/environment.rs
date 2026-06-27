//! `impl Inspect for Environment` — the fog volume's editable properties (placement,
//! density/tint, and the advanced scattering-model knobs). The name heading + kind
//! sub-title stay in the dispatch wrapper.

use super::props::{Inspect, Props};
use crate::scene::environment::Environment;
use crate::ui::theme::icon;

impl Inspect for Environment {
    fn inspect(&mut self, p: &mut Props) {
        // Env defaults = the `from_profile` rest constants (the density template lives on
        // the profile, which the instance doesn't keep → Density gets no revert arrow).
        const D_COLOR: [f32; 3] = [0.7, 0.72, 0.78];
        const D_ANISO: f32 = 0.25;
        const D_UNIFORM: f32 = 0.6;
        const D_CLUSTER: f32 = 0.0;

        p.group("Transform", icon::INSPECTOR, true, |p| {
            p.vec3("Center", &mut self.center).speed(0.1);
            // Size keeps its W/H/D prefixes + range, stacked the same way.
            p.vec3("Size", &mut self.size).prefixes(["W", "H", "D"]).range(0.1..=500.0).speed(0.1);
        });

        p.group("Volume", icon::ENVIRONMENT, true, |p| {
            p.f32("Density", &mut self.density).speed(0.005).range(0.0..=4.0);
            p.color("Tint", &mut self.color, Some(D_COLOR));
            p.advanced("volume", |p| {
                p.f32("Anisotropy", &mut self.anisotropy)
                    .speed(0.005)
                    .range(-0.95..=0.95)
                    .default(D_ANISO)
                    .tip("Henyey-Greenstein g (forward scattering > 0)");
                p.f32("Uniformity", &mut self.uniformity)
                    .range(0.0..=1.0)
                    .slider()
                    .default(D_UNIFORM)
                    .tip(
                        "1 = smooth even haze · 0 = clusters of smoke/clouds (dense pockets \
                         scatter brighter, with clear gaps between)",
                    );
                p.f32("Cluster contrast", &mut self.cluster_contrast)
                    .range(0.0..=1.0)
                    .slider()
                    .default(D_CLUSTER)
                    .tip(
                        "How much brighter/denser the clusters are vs the haze (and how clear \
                         the gaps). Higher = pockets pop harder. Pairs with low density.",
                    );
            });
        });
    }
}
