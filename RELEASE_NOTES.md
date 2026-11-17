## NyashkEQ 1.0.6

- Дзвони на високих частотах тепер мають ту саму форму, що й аналогові, аж до Найквіста: на 44.1/48 кГц високий дзвін більше не «стискається» (cramping). Наприклад, дзвін на 12 кГц +12 дБ дає на 16 кГц +8.5 дБ, і на 96 кГц теж +8.6 дБ (раніше на 44.1 кГц було +4.4 дБ).

## NyashkEQ 1.0.5

- ANALOG switches without a click (the signal now always takes the same oversampled path, the saturation fades in / out), and all switch fades are S-shaped over 10 ms (a large EQ boost switched in or out no longer leaves a faint corner).

## NyashkEQ 1.0.4

- **Click-free switching:** Bypass, EQ IN and the high- / low-pass on-off buttons now fade over 5-10 ms, and the filters keep running underneath, so switching during playback no longer clicks (bypass used to cut over hard and the filters restarted from a stale state).
- New behaviour tests: bell bandwidth follows Q, peak gain exact up to 12 kHz at 44.1 / 48 / 96 kHz, filter corners at -3 dB with 12 / 18 dB per octave, analog stage clean at low level and mild when hot, fast automation without zipper noise.

## NyashkEQ 1.0.3

- Frequency grid labels read 1k / 2k / 10k instead of 1.00k / 2.00k / 10.00k; the level scale shows a plain 0 instead of +0.

## NyashkEQ 1.0.2

- **Points stay inside the graph.** At 20 Hz, 20 kHz or full boost/cut the band points sat half on the frame, and their labels and values went outside the graph or over the hint text. Points are now kept inside, the label moves below the point when there is no room above, the value on hover goes wherever there is room (never outside), and both have a dark backing so they stay readable where the curve runs through them.
- The "drag the points" hint moved to the header, so the points no longer run over it.

## NyashkEQ 1.0.1

- **Alt-tab fix (Linux):** after alt-tab, minimising or covering the window, parts of the picture (backgrounds, knobs, images) could stay blank until something on them moved. The window is now drawn again in full whenever the system throws its contents away, and pictures are no longer lost while the window is hidden or a frame is skipped.
- Smoother images in the CPU renderer (used when Vulkan is not available): pictures drawn smaller than their real size, such as knobs and photos, are now scaled with mipmaps instead of picking single pixels, so they no longer look grainy or jagged.
- **Display fix:** the built-in CPU renderer (used when Vulkan is not available) drew pixels on the shared edges of triangles twice, which showed as brighter seams and specks in see-through fills (spectra, curves, bars). It now uses an exact fill rule like a GPU: every pixel is drawn once, without holes.
- The live spectrum is drawn one column per pixel (loudest bin) as a clean solid fill: no more over-bright, flickering top edge or stray dots, and the grid stays visible over it.
- Value boxes have a small gap between them, and a value that would not fit its box is drawn smaller instead of running over the frame.
- Clicking a value box and pressing Enter no longer changes a value whose unit matters (for example "1.03 s" was read back as 1.03 ms).
- No more "-0.0" read-outs.
- New automatic test: every parameter's text is typed back over its whole range and must give the same value.

## NyashkEQ 1.0.0

First release. VST3 for Linux, Windows and macOS.
