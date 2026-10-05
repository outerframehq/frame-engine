// The public editor: no extra modules. Private builds with extra features
// call `frame_editor::run` themselves with their own modules.
fn main() {
    frame_editor::run(Vec::new());
}
