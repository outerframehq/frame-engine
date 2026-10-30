// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// What still water mirrors of the sky looking in `direction`, over `colour`,
// the sky's own gradient there: the sky's flat clouds. Compiled after
// shader.wgsl (see `ENTITY_WGSL` in lib.rs). A module that draws clouds of
// its own gives the water a function of its own instead
// (`EditorModule::reflection_wgsl`), compiled in this one's place, so the
// water mirrors its clouds.
fn reflected_sky(colour: vec3<f32>, direction: vec3<f32>) -> vec3<f32> {
    return with_clouds(colour, direction);
}
