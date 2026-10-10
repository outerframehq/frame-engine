// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The public editor: no extra modules. Private builds with extra features
// call `frame_editor::run` themselves with their own modules.
// Counts the memory in use, for the performance overlay (F3).
#[global_allocator]
static ALLOC: frame_editor::CountingAlloc = frame_editor::CountingAlloc;

fn main() {
    frame_editor::run(Vec::new());
}
