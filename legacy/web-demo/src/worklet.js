/* =====================================================================
   Código do AudioWorklet (roda na thread de áudio). É montado como texto
   para a página continuar sendo um arquivo só; o detector entra por
   makeDetector.toString().
   ===================================================================== */
function workletSource() {
  return makeDetector.toString() + '\n' + [
    'class TonicaProc extends AudioWorkletProcessor {',
    '  constructor(o) {',
    '    super();',
    '    const p = o.processorOptions;',
    '    this.D = p.D; this.W = p.W; this.hop = p.hop;',
    '    this.det = makeDetector(sampleRate / p.D, p.W);',
    '    this.ring = new Float32Array(p.W); this.lin = new Float32Array(p.W);',
    '    this.wp = 0; this.acc = 0; this.accN = 0; this.since = 0;',
    '    this.targets = []; this.tid = 0; this.probe = null;',
    '    this.port.onmessage = (e) => {',
    '      const d = e.data;',
    '      if (d.targets) { this.targets = d.targets; this.tid = d.tid; }',
    '      if (d.probe) this.probe = { t0: d.probe, thr: d.thr };',
    '    };',
    '  }',
    '  process(inputs) {',
    '    const ch = inputs[0] && inputs[0][0];',
    '    if (!ch) return true;',
    '    const D = this.D, W = this.W, ring = this.ring, n = ch.length;',
    '    for (let i = 0; i < n; i++) {',
    '      this.acc += ch[i];',
    '      if (++this.accN === D) {',
    '        ring[this.wp] = this.acc / D; this.acc = 0; this.accN = 0;',
    '        if (++this.wp === W) this.wp = 0;',
    '        if (++this.since >= this.hop) { this.since = 0; this.analyze(i); }',
    '      }',
    '    }',
    '    const pr = this.probe;',
    '    if (pr) {                                   // medição de latência: acha o clique',
    '      for (let i = 0; i < n; i++) {',
    '        const ts = currentTime + i / sampleRate;',
    '        if (ts >= pr.t0 && (ch[i] > pr.thr || ch[i] < -pr.thr)) {',
    '          this.port.postMessage({ probe: ts - pr.t0 }); this.probe = null; break;',
    '        }',
    '      }',
    '      if (this.probe && currentTime - pr.t0 > 1) { this.port.postMessage({ probe: -1 }); this.probe = null; }',
    '    }',
    '    return true;',
    '  }',
    '  analyze(i) {',
    '    const W = this.W, wp = this.wp;',
    '    this.lin.set(this.ring.subarray(wp), 0);',
    '    this.lin.set(this.ring.subarray(0, wp), W - wp);',
    '    const r = this.det.analyze(this.lin, this.hop);',
    '    const tg = this.targets, p = new Array(tg.length);',
    '    for (let k = 0; k < tg.length; k++) p[k] = this.det.presence(tg[k]);',
    '    this.port.postMessage({ t: currentTime + i / sampleRate, f: r.f, c: r.c, rms: r.rms, rmsS: r.rmsS, p: p, tid: this.tid });',
    '  }',
    '}',
    "registerProcessor('tonica-proc', TonicaProc);"
  ].join('\n');
}

if (typeof module !== 'undefined' && module.exports) module.exports = { workletSource: workletSource };
