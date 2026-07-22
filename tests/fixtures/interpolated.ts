// A shader that injects a module chunk via `${...}` and calls into it. glslint
// can't reconstruct a faithful unit, so it checks with lints only and notes it.
const fs = glsl`#version 300 es
precision highp float;
${lightingModule}
out vec4 fragColor;
void main() {
  fragColor = computeLighting();
}
`;

// An interpolated shader that still trips the source-level ES3 legacy lints:
// `varying` and `gl_FragColor` are flagged even though glslang is skipped.
const legacy = glsl`#version 300 es
${chunk}
varying vec2 vUv;
void main() { gl_FragColor = vec4(vUv, 0.0, 1.0); }
`;
