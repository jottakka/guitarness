import pathlib, sys
root = pathlib.Path(__file__).parent
src = root / 'src'
order = ['dsp.js', 'tracker.js', 'worklet.js', 'theory.js', 'core.js', 'audio.js', 'modes.js', 'main.js']
js = "(function () {\n'use strict';\n" + "\n".join((src / f).read_text() for f in order) + "\n})();"
assert '</script' not in js.lower()
html = (src / 'index.html').read_text().replace('/*__CSS__*/', (src / 'style.css').read_text()).replace('/*__JS__*/', js)
out = root / 'dist' / 'tonica.html'
out.write_text(html)
print('ok', out, len(html), 'bytes;', html.count('\n'), 'linhas')
