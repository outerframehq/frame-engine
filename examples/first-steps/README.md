# First steps

A small project to open before you make your own. In the editor's launcher press **Open existing project…** and pick this folder, then press **P**.

- A row of entities, each running one script from [GETTING_STARTED.md](../../GETTING_STARTED.md): one spins, one bobs, one glows while **W** is held, one makes a new entity every two seconds for the first minute, each a little further behind it, and one paces along the row behind.
- A few low steps on the right to walk up and jump onto.
- A character with a camera. While playing in the editor, **WASD** walks it, **Shift** runs, **Space** jumps. **Play scene** sees from its eyes: click to take the mouse, **Esc** to free it.

Open the **Script Editor** tab, pick a script, and change a number while it plays. Then make a project of your own with **Create…**.

The files here are written by `first_steps_world` in `crates/frame-editor/src/lib.rs`, and a test keeps them in step: after changing that function, run `FRAME_WRITE_EXAMPLES=1 cargo test -p frame-editor example_project -- --test-threads=1` to regenerate them.
