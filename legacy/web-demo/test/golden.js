// Gera entradas (WAV) e saídas de referência (JSON + CSV) para portar o detector para outra linguagem.
// Uso: node test/golden.js [pasta]   (determinístico: PRNG com semente fixa)
// <caso>.wav          sinal "de microfone" (antes dos filtros), mono, 16 bits, 48 kHz
// <caso>.json         parâmetros, notas tocadas, alvos e os eventos que o pipeline JS produz para esse WAV
// <caso>.quadros.csv  saída do detector quadro a quadro (t, f, c, rms, rmsS, tid, p), para achar onde um porte diverge
// Pipeline: filtros do grafo -> worklet (decimação + detector) -> rastreador.
const fs = require('fs'), vm = require('vm'), path = require('path'), crypto = require('crypto');
const SRC = ['dsp.js', 'tracker.js', 'worklet.js'].map(f => fs.readFileSync(path.join(__dirname, '../src', f), 'utf8')).join('\n');
const sb = { console };
vm.createContext(sb);
vm.runInContext(SRC + '\nthis.API = { makeTracker, workletSource, ksPluck };', sb);
const { makeTracker, workletSource, ksPluck } = sb.API;
const SR = 48000, OUT = process.argv[2] || path.join(__dirname, 'golden');
fs.mkdirSync(OUT, { recursive: true });

function rng(a) { return function () { a |= 0; a = a + 0x6D2B79F5 | 0; let t = Math.imul(a ^ a >>> 15, 1 | a); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; }; }
const mtof = m => 440 * Math.pow(2, (m - 69) / 12);
function biquad(type, f0, Q, sr) {            // fórmulas RBJ, as mesmas do BiquadFilterNode
  const w0 = 2 * Math.PI * f0 / sr, c = Math.cos(w0), al = Math.sin(w0) / (2 * Q);
  let b0, b1, b2;
  if (type === 'hp') { b0 = (1 + c) / 2; b1 = -(1 + c); b2 = (1 + c) / 2; } else { b0 = (1 - c) / 2; b1 = 1 - c; b2 = (1 - c) / 2; }
  const a0 = 1 + al;
  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: -2 * c / a0, a2: (1 - al) / a0 };
}
function filt(q, x) {
  let x1 = 0, x2 = 0, y1 = 0, y2 = 0;
  for (let i = 0; i < x.length; i++) { const y = q.b0 * x[i] + q.b1 * x1 + q.b2 * x2 - q.a1 * y1 - q.a2 * y2; x2 = x1; x1 = x[i]; y2 = y1; y1 = y; x[i] = y; }
}
function mic(plucks, dur, seed) {             // cordas sintéticas + estalo da palhetada + ruído de fundo
  const r = rng(seed), x = new Float32Array(Math.floor(dur * SR));
  for (const p of plucks) {
    const tone = ksPluck(SR, mtof(p.midi), Math.min(3, dur - p.t), r), off = Math.floor(p.t * SR);
    const dEnd = p.damp ? Math.floor((p.damp - p.t) * SR) : Infinity;
    for (let i = 0; i < tone.length && off + i < x.length; i++) {
      let g = p.amp; if (i > dEnd) g *= Math.max(0, 1 - (i - dEnd) / (0.03 * SR));
      x[off + i] += tone[i] * g;
    }
    const nb = Math.floor(0.006 * SR);
    for (let i = 0; i < nb && off + i < x.length; i++) x[off + i] += (r() * 2 - 1) * p.amp * 0.5 * (1 - i / nb);
  }
  for (let i = 0; i < x.length; i++) x[i] += (r() * 2 - 1) * 0.002 * 1.732;
  return x;
}
function wav(file, x) {                       // PCM 16 bits mono; devolve o sinal já quantizado (o que o porte vai ler)
  const n = x.length, b = Buffer.alloc(44 + n * 2), q = new Float32Array(n);
  b.write('RIFF', 0); b.writeUInt32LE(36 + n * 2, 4); b.write('WAVEfmt ', 8); b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(SR, 24); b.writeUInt32LE(SR * 2, 28); b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34); b.write('data', 36); b.writeUInt32LE(n * 2, 40);
  for (let i = 0; i < n; i++) { const v = Math.max(-32768, Math.min(32767, Math.round(x[i] * 32767))); b.writeInt16LE(v, 44 + i * 2); q[i] = v / 32768; }
  fs.writeFileSync(file, b); return { q, sha: crypto.createHash('sha256').update(b).digest('hex') };
}
function pipeline(x, alvos) {
  filt(biquad('hp', 62, 0.707, SR), x); filt(biquad('lp', 2600, 0.707, SR), x); filt(biquad('lp', 2600, 0.707, SR), x);
  const D = Math.max(1, Math.round(SR / 24000)), fsd = SR / D, W = 2 * Math.round(fsd * 0.0427 / 2), hop = Math.round(fsd * 0.0107);
  const ctx = { sampleRate: SR, currentTime: 0, out: [], console };
  ctx.AudioWorkletProcessor = class { constructor() { this.port = { postMessage: m => ctx.out.push(m), onmessage: null }; } };
  ctx.registerProcessor = (n, c) => { ctx.cls = c; };
  vm.createContext(ctx); vm.runInContext(workletSource(), ctx);
  const proc = new ctx.cls({ processorOptions: { D, W, hop } });
  const evs = [], tr = makeTracker(e => evs.push({ t: +e.t.toFixed(5), tipo: e.type, midi: e.midi, via: e.via, ataque: e.ataque }));
  let si = 0, tid = 0;
  const frames = [];
  for (let b = 0; (b + 1) * 128 <= x.length; b++) {
    ctx.currentTime = b * 128 / SR;
    while (si < alvos.length && alvos[si].t <= ctx.currentTime) { tid++; proc.port.onmessage({ data: { targets: alvos[si].midis.map(mtof), tid } }); tr.setTargets(alvos[si].midis, tid, ctx.currentTime); si++; }
    proc.process([[x.subarray(b * 128, (b + 1) * 128)]]);
    for (const m of ctx.out) { frames.push(m); tr.push(m); }
    ctx.out.length = 0;
  }
  return { parametros: { taxa: SR, decimacao: D, janela: W, passo: hop, bloco: 128, filtros: 'RBJ: passa-altas 62 Hz Q 0,707; dois passa-baixas 2600 Hz Q 0,707' }, quadros: frames.length, eventos: evs, frames };
}

const esc = [40, 43, 45, 47, 50, 52, 55, 57, 59, 62, 64, 67, 64, 62, 59, 57, 55, 52, 50, 47, 45, 43, 40];
const casos = [];
{ const pl = []; let t = 0.6; for (let m = 40; m <= 76; m++) { pl.push({ t: +t.toFixed(3), midi: m, amp: 0.2, damp: +(t + 0.34).toFixed(3) }); t += 0.45; }
  casos.push({ nome: 'A-isoladas', desc: 'Mi2 a Mi5, uma nota por vez, abafada antes da próxima. Sem alvo: só o caminho mpm.', pl, dur: t + 0.4, alvos: [] }); }
{ const pl = []; let t = 0.6; for (const m of esc) { pl.push({ t: +t.toFixed(3), midi: m, amp: 0.2 }); t += 0.5; }
  casos.push({ nome: 'B-soando', desc: 'Notas que continuam soando (cordas soltas). Sem alvo.', pl, dur: t + 0.4, alvos: [] }); }
{ const pl = [], al = []; let t = 0.6; for (const m of esc) { pl.push({ t: +t.toFixed(3), midi: m, amp: 0.2 }); al.push({ t: +(t - 0.18).toFixed(3), midis: [m] }); t += 0.3; }
  casos.push({ nome: 'C-alvo-rapido', desc: 'Notas soando, uma a cada 0,3 s, com o alvo avisado 180 ms antes (modo guiado).', pl, dur: t + 0.4, alvos: al }); }
{ const pl = [], al = []; let t = 0.6; for (const m of esc) { pl.push({ t: +t.toFixed(3), midi: m, amp: pl.length % 2 ? 0.1 : 0.25 }); al.push({ t: +(t - 0.3).toFixed(3), midis: [m] }); t += 0.5; }
  casos.push({ nome: 'D-forte-fraco', desc: 'Nota fraca sobre nota forte ainda soando: o ponto fraco conhecido do detector (JS perde 4 de 23).', pl, dur: t + 0.4, alvos: al }); }
{ const pl = [], al = []; let t = 0.6; for (const m of [45, 48, 50, 52, 55, 57, 60, 62, 64]) { pl.push({ t: +t.toFixed(3), midi: m, amp: 0.2, damp: +(t + 0.4).toFixed(3) }); al.push({ t: +(t - 0.3).toFixed(3), midis: [m + 12] }); t += 0.5; }
  casos.push({ nome: 'F-oitava-errada', desc: 'O alvo é a oitava de cima da nota tocada: não pode sair nenhum evento via "alvo".', pl, dur: t + 0.4, alvos: al }); }

const resumo = [];
casos.forEach((c, i) => {
  const w = wav(path.join(OUT, c.nome + '.wav'), mic(c.pl, c.dur, 11 + i));
  const r = pipeline(w.q, c.alvos), notas = r.eventos.filter(e => e.tipo === 'nota');
  let ok = 0;
  c.pl.forEach((p, k) => { const fim = c.pl[k + 1] ? c.pl[k + 1].t : p.t + 0.6; if (notas.some(e => e.t >= p.t - 0.005 && e.t < fim && e.midi === p.midi)) ok++; });
  fs.writeFileSync(path.join(OUT, c.nome + '.json'), JSON.stringify({ nome: c.nome, descricao: c.desc, wav_sha256: w.sha, parametros: r.parametros, tocadas: c.pl, alvos: c.alvos, quadros: r.quadros, eventos: r.eventos }, null, 1));
  const csv = ['t,f,c,rms,rmsS,tid,p'];
  for (const m of r.frames) csv.push([m.t.toFixed(6), m.f.toFixed(4), m.c.toFixed(5), m.rms.toExponential(6), m.rmsS.toExponential(6), m.tid, Array.from(m.p, v => v.toFixed(5)).join(';')].join(','));
  fs.writeFileSync(path.join(OUT, c.nome + '.quadros.csv'), csv.join('\n') + '\n');
  resumo.push(`${c.nome.padEnd(16)} ${String(ok).padStart(2)}/${c.pl.length} notas tocadas reconhecidas; ${notas.filter(e => e.via === 'alvo').length} eventos via alvo, ${notas.filter(e => e.via === 'mpm').length} via mpm; ${r.quadros} quadros`);
});
fs.writeFileSync(path.join(OUT, 'RESUMO.txt'), resumo.join('\n') + '\n');
console.log(resumo.join('\n'));
