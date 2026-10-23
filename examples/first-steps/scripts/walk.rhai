// Pace along X, four metres either side of the middle. The script
// places the entity itself, so it keeps the velocity at zero.
vel = vec3(0.0, 0.0, 0.0);
pos.x = sin(t / 30.0) * 4.0;
