// Shaders whose stage isn't obvious from a `gl_Position`/filename signal — the
// stage inference must still pick a stage that doesn't misreport valid builtins.

// A transform-feedback vertex shader: no `gl_Position` (rasterizer discard), but
// `gl_VertexID` is vertex-only, so it must validate under the vertex stage.
const update = glsl`#version 300 es
in float inValue;
out float outValue;
void main() {
  outValue = inValue * float(gl_VertexID);
}
`;

// A plain fragment shader with no distinctive builtin — the fragment default.
const shade = glsl`#version 300 es
precision highp float;
uniform vec3 uColor;
out vec4 fragColor;
void main() {
  fragColor = vec4(uColor, 1.0);
}
`;
