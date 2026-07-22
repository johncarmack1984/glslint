// The `/* glsl */` marker form, used by prettier/lit-style tooling for editors
// that highlight the template but don't run a `glsl` tag function.
export const source =
  /* glsl */ `#version 300 es
precision highp float;
out vec4 fragColor;
void main() {
  fragColor = vec4(1.0);
}
`;
