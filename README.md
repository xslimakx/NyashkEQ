<div align="center">

# NyashkEQ

4-band parametric equalizer VST3 plugin with draggable nodes over a live spectrum, HP / LP and mid / side mode.

[![Build](https://github.com/xslimakx/NyashkEQ/actions/workflows/build.yml/badge.svg)](https://github.com/xslimakx/NyashkEQ/actions/workflows/build.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Format](https://img.shields.io/badge/format-VST3-8a2be2)

[**Download**](https://github.com/xslimakx/NyashkEQ/releases/latest)

</div>

<p align="center"><img src="docs/img/nyashkeq.jpg" width="720"></p>

## Install
Use the Nyashk Launcher, or unzip the `.vst3` bundle from the latest release into your VST3 folder.

## Build
```
cargo xtask bundle -p nyashkeq --release
```
The bundle appears in `target/bundled/`.

## License
MIT
