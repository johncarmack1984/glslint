# @glslint/cli

A GLSL checker and language server for the shaders WebGL toolkits actually ship, distributed as a prebuilt binary. No Rust toolchain needed.

Stock GLSL tools choke on these files because they aren't standalone translation units: WebGL has no `#include`, so libraries like luma.gl/deck.gl, maplibre-gl-js, and ShaderToy each assemble shaders in JS at build time — a raw `.glsl` references UBO instances (`wind.*`), deck builtins (`project_position_to_clipspace`), maplibre's `#pragma maplibre:` properties and shared `projectTile`/`_prelude` library, or ShaderToy's implicit `iTime`, none of which are in the file. glslint reconstructs the complete unit — discovering the project's shared library and expanding its dialect — validates it with the Khronos glslangValidator reference compiler, and maps every diagnostic back to the original file and line. The core works on an unfamiliar project with zero config; ecosystem specifics live in data (bundled presets and a project's own `glslint.toml`).

## Requires glslangValidator

glslint shells out to `glslangValidator`, the Khronos GLSL reference compiler. It is **not** bundled in this package, so install it once:

```sh
brew install glslang            # macOS (provides glslangValidator and the newer `glslang`)
sudo apt install glslang-tools  # Debian / Ubuntu
```

On Windows it ships with the [Vulkan SDK](https://vulkan.lunarg.com/). glslint finds it on `PATH`; set `GLSLINT_GLSLANG` to point at a specific binary. If it is missing, glslint names the install command for your platform rather than reporting a bare "not found".

## Install

```sh
npm install --save-dev @glslint/cli
```

The binary arrives through a per-platform optional dependency (`@glslint/darwin-arm64` and friends), so you download one binary rather than all of them. Prebuilt platforms: macOS arm64 and x64, Linux x64, Windows x64. On anything else, install with `cargo install glslint`.

## Usage

The package installs a `glslint` command, so the scope only appears when you install it:

```sh
npx glslint check src/shaders/draw.vert.glsl   # one-shot, exit 1 on errors
npx glslint check 'src/**/*.ts'                # lint GLSL in JS/TS tagged templates
npx glslint lsp                                # language server over stdio
```

`check` resolves config by walking up for a `glsl-lsp.toml`. With no config at all it derives each shader's modules from your `new Model({ vs, fs, modules })` calls, and falls back to sibling `*Uniforms.glsl` discovery.

To resolve the binary yourself, for example to launch the language server from an editor extension without the wrapper process:

```js
const { binaryPath } = require('@glslint/cli');
spawn(binaryPath(), ['lsp']);
```

## Documentation

Config format, the assembler, the diagnostic mapping, and why glslangValidator instead of naga: [github.com/johncarmack1984/glslint](https://github.com/johncarmack1984/glslint#readme).

## License

Dual-licensed under either MIT or Apache-2.0, at your option.
