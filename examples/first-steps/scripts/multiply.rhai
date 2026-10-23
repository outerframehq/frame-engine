// Make a new entity every two seconds for the first minute, each one
// 1.5 m further behind this one, so they line up instead of piling up.
// A spawn is a request, and nothing limits how often you make it, so it
// is guarded twice.
if t % 60.0 == 0.0 && t < 1800.0 {
    spawn_at = vec3(pos.x, pos.y, pos.z - 1.5 * (t / 60.0));
    spawn_new = true;
}
