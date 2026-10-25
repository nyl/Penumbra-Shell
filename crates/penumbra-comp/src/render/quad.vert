#version 100
// # Mathematical Formulation
// Full-screen / viewport-aligned billboard quad in Normalized Device Coordinates (NDC):
// Input vertex position a_position in [-1.0, 1.0] x [-1.0, 1.0]
// Texture coordinate mapping: v_coords = (a_position + 1.0) * 0.5 in [0.0, 1.0] x [0.0, 1.0]
// gl_Position = vec4(a_position, 0.0, 1.0)
//
// # Performance Considerations
// Pre-computed static 4-vertex triangle strip (8 floats) cached in a single VBO.
// Zero per-frame vertex overhead; completely bypasses matrix multiplications on mobile GPUs.

attribute vec2 a_position;
varying vec2 v_coords;

void main() {
    v_coords = (a_position + 1.0) * 0.5;
    gl_Position = vec4(a_position, 0.0, 1.0);
}
