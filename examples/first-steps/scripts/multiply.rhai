// Make a new entity where this one stands every two seconds, for the
// first minute. A spawn is a request, and nothing limits how often you
// make it, so it is guarded twice.
if t % 60.0 == 0.0 && t < 1800.0 {
    spawn_at = pos;
    spawn_new = true;
}
