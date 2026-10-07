// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The public editor: no extra modules. Private builds with extra features
// call `frame_editor::run` themselves with their own modules.
fn main() {
    frame_editor::run(Vec::new());
}
