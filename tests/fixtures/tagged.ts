import { Model } from "@luma.gl/engine";

// A self-contained fragment shader with one deliberate error: `nope` is undeclared.
const fs = glsl`#version 300 es
precision highp float;
out vec4 fragColor;
void main() {
  fragColor = vec4(nope, 0.0, 0.0, 1.0);
}
`;

// A clean vertex shader — no diagnostics expected.
const vs = glsl`#version 300 es
in vec3 positions;
void main() {
  gl_Position = vec4(positions, 1.0);
}
`;

export function make(device: unknown) {
  return new Model(device, { vs, fs });
}
