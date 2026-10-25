#version 100
precision mediump float;

varying vec2 v_coords;
uniform sampler2D u_tex;
uniform vec2 u_half_pixel;
uniform float u_offset;

// # Mathematical Formulation
// Downsample pass evaluates 4 diagonal cross samples offset by distance d:
// C(u, v) = 1/4 * sum_{dx in {-d, d}, dy in {-d, d}} I(u + dx * half_pixel.x, v + dy * half_pixel.y)
// Utilizing hardware bilinear filtering (GL_LINEAR) during 1/4 downsampling.
//
// # Performance Considerations
// Downscaling from 412x915 to 103x228 reduces pixel operations by ~93.75%.
// Single draw call with 4 texture lookups per pixel (~94k texture reads total).

void main() {
    vec2 offset = u_half_pixel * u_offset;
    vec4 sum = vec4(0.0);
    sum += texture2D(u_tex, v_coords + vec2(-offset.x, -offset.y));
    sum += texture2D(u_tex, v_coords + vec2( offset.x, -offset.y));
    sum += texture2D(u_tex, v_coords + vec2(-offset.x,  offset.y));
    sum += texture2D(u_tex, v_coords + vec2( offset.x,  offset.y));
    gl_FragColor = sum * 0.25;
}
