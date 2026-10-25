#version 100
precision mediump float;

varying vec2 v_coords;
uniform sampler2D u_scene_tex;  // Sharp scene texture (unit 0)
uniform sampler2D u_blur_tex;   // Diffuse blurred texture (unit 1)
uniform float u_progress;       // Gesture progress t in [0.0, 1.0]

// # Mathematical Formulation
// 1. Cross-fade blend factor via Hermite interpolation:
//    w(t) = smoothstep(0.05, 0.95, t)
// 2. Linear texture interpolation between sharp scene and diffused blur:
//    C_scene = mix(C_sharp, C_diffuse, w(t))
// 3. Dynamic Material 3 dark scrim tint (#0b0a0e):
//    C_scrim = vec3(11.0 / 255.0, 10.0 / 255.0, 14.0 / 255.0) = vec3(0.043137, 0.039216, 0.054902)
//    alpha_scrim = clamp(u_progress * 0.40, 0.0, 0.40)
//    C_final = mix(C_scene.rgb, C_scrim, alpha_scrim)
//
// # Performance Considerations
// Progressive cross-fade combines texture blending and scrim compositing into a single
// unified fragment shader pass, avoiding extra framebuffer roundtrips or multi-layer blending passes.

void main() {
    float t = clamp(u_progress, 0.0, 1.0);
    float blend_factor = smoothstep(0.05, 0.95, t);

    vec4 sharp_col = texture2D(u_scene_tex, v_coords);
    vec4 blur_col = texture2D(u_blur_tex, v_coords);
    vec4 scene_col = mix(sharp_col, blur_col, blend_factor);

    vec3 scrim_col = vec3(0.043137, 0.039216, 0.054902); // #0b0a0e
    float scrim_alpha = t * 0.40;
    vec3 final_rgb = mix(scene_col.rgb, scrim_col, scrim_alpha);

    gl_FragColor = vec4(final_rgb, 1.0);
}
