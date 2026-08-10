# Changelog

## [0.8.1](https://github.com/johncarmack1984/glslint/compare/v0.8.0...v0.8.1) (2026-08-10)


### Bug Fixes

* **vscode:** keep the language server binary version-matched to the extension ([#52](https://github.com/johncarmack1984/glslint/issues/52)) ([d81e54f](https://github.com/johncarmack1984/glslint/commit/d81e54fa213d25bb2d9ddca0e34edf08b560bee7))

## [0.8.0](https://github.com/johncarmack1984/glslint/compare/v0.7.0...v0.8.0) (2026-08-09)


### Features

* **dialect:** generalizable core + presets as data ([#46](https://github.com/johncarmack1984/glslint/issues/46)) ([80c4f81](https://github.com/johncarmack1984/glslint/commit/80c4f81d68501ce451d3b5d395b1eefe66e7b09f))
* **include:** resolve GL_GOOGLE_include_directive #include directives ([#49](https://github.com/johncarmack1984/glslint/issues/49)) ([ae8e355](https://github.com/johncarmack1984/glslint/commit/ae8e355707518aeb5f3e104033b7bcf0344a320d))
* **preset:** express deck.gl as a preset and unify the config filename ([#48](https://github.com/johncarmack1984/glslint/issues/48)) ([2ce01d5](https://github.com/johncarmack1984/glslint/commit/2ce01d503234a8f23e028502f253107cdb5b8fa7))

## [0.7.0](https://github.com/johncarmack1984/glslint/compare/v0.6.0...v0.7.0) (2026-08-09)


### Features

* **dialect:** inject maplibre's shared shader library and detect it on disk ([#44](https://github.com/johncarmack1984/glslint/issues/44)) ([3cdcb3a](https://github.com/johncarmack1984/glslint/commit/3cdcb3a6de509c67bbfbe74cef15d87d2aaf9389))

## [0.6.0](https://github.com/johncarmack1984/glslint/compare/v0.5.0...v0.6.0) (2026-08-09)


### Features

* **dialect:** resolve ecosystem shader pragmas via a dialect layer ([#42](https://github.com/johncarmack1984/glslint/issues/42)) ([299e2dc](https://github.com/johncarmack1984/glslint/commit/299e2dc3362fffe5e79d76efd372716a131caf99))
* **homebrew:** distribute glslint through a Homebrew tap ([#40](https://github.com/johncarmack1984/glslint/issues/40)) ([7565178](https://github.com/johncarmack1984/glslint/commit/756517872972d7bfce1619f82ffb2a56eac2a831))

## [0.5.0](https://github.com/johncarmack1984/glslint/compare/v0.4.1...v0.5.0) (2026-08-09)


### Features

* **vscode:** add extension icon ([#38](https://github.com/johncarmack1984/glslint/issues/38)) ([7d37fc8](https://github.com/johncarmack1984/glslint/commit/7d37fc8e9f0f46158559897c50b26395afe0b372))

## [0.4.1](https://github.com/johncarmack1984/glslint/compare/v0.4.0...v0.4.1) (2026-08-09)


### Bug Fixes

* **assemble:** detect spelled-out .vertex/.fragment/.compute stage names ([#36](https://github.com/johncarmack1984/glslint/issues/36)) ([e93da5d](https://github.com/johncarmack1984/glslint/commit/e93da5d48d13ceffae8680804bcd9a088b7e8eb9))

## [0.4.0](https://github.com/johncarmack1984/glslint/compare/v0.3.1...v0.4.0) (2026-07-22)


### Features

* lint GLSL in JS/TS tagged template literals ([#28](https://github.com/johncarmack1984/glslint/issues/28)) ([db05149](https://github.com/johncarmack1984/glslint/commit/db0514966d404d931e7e0caa25f0ea2c7fa67f53))

## [0.3.1](https://github.com/johncarmack1984/glslint/compare/v0.3.0...v0.3.1) (2026-07-20)


### Bug Fixes

* publish the npm packages over OIDC trusted publishing ([#23](https://github.com/johncarmack1984/glslint/issues/23)) ([d7fa0bb](https://github.com/johncarmack1984/glslint/commit/d7fa0bbd42d21abac7d8d330ef39c7f4ea5e96e6))

## [0.3.0](https://github.com/johncarmack1984/glslint/compare/v0.2.0...v0.3.0) (2026-07-20)


### Features

* npm distribution channel ([#19](https://github.com/johncarmack1984/glslint/issues/19)) ([5ede062](https://github.com/johncarmack1984/glslint/commit/5ede0622aea619de2ed08cfc9ce98912085a44d3))

## [0.2.0](https://github.com/johncarmack1984/glslint/compare/v0.1.3...v0.2.0) (2026-06-24)


### Features

* auto-derive shader/module bindings from new Model() calls ([#4](https://github.com/johncarmack1984/glslint/issues/4)) ([196b702](https://github.com/johncarmack1984/glslint/commit/196b7020c02a47224bbd120da14dda3e2d259111))

## [0.1.3](https://github.com/johncarmack1984/glslint/compare/v0.1.2...v0.1.3) (2026-06-24)


### Continuous Integration

* automate releases with release-please ([7f1e2f2](https://github.com/johncarmack1984/glslint/commit/7f1e2f2a4acda9c1e540dd60ad233d69a0224268))
* fix release tag naming and binary-build wiring ([#2](https://github.com/johncarmack1984/glslint/issues/2)) ([2ea3ec5](https://github.com/johncarmack1984/glslint/commit/2ea3ec5d386e0150f3fef460618f437a7c7d3431))

## [0.1.2](https://github.com/johncarmack1984/glslint/compare/glslint-v0.1.1...glslint-v0.1.2) (2026-06-24)


### Continuous Integration

* automate releases with release-please ([7f1e2f2](https://github.com/johncarmack1984/glslint/commit/7f1e2f2a4acda9c1e540dd60ad233d69a0224268))
