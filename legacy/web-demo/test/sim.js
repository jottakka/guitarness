// Simulação offline: violão sintético -> filtros do grafo -> worklet real -> rastreador.
const fs = require('fs'), vm = require('vm'), path = require('path');
const SRC = ['dsp.js', 'tracker.js', 'worklet.js'].map(f => fs.readFileSync(path.join(__dirname, '../src', f), 'utf8')).join('\n');
const sb = { console };
vm.createContext(sb);
vm.runInContext(SRC + '\nthis.API = { makeDetector, makeTracker, workletSource, ksPluck, detectKey, fft };', sb);
const { makeTracker, workletSource, ksPluck, detectKey } = sb.API;

const SR = +(process.env.SR || 48000);
function rng(a) { return function () { a |= 0; a = a + 0x6D2B79F5 | 0; let t = Math.imul(a ^ a >>> 15, 1 | a); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; }; }
const mtof = m => 440 * Math.pow(2, (m - 69) / 12);

function biquad(type, f0, Q, sr) {
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

// plucks: [{t, midi, amp, damp?}]
function render(plucks, dur, o) {
  o = Object.assign({ noise: 0.002, phone: false, seed: 7 }, o);
  const r = rng(o.seed), x = new Float32Array(Math.floor(dur * SR));
  for (const p of plucks) {
    const tone = ksPluck(SR, mtof(p.midi) * (p.detune || 1), Math.min(3, dur - p.t), r), off = Math.floor(p.t * SR);
    const dEnd = p.damp ? Math.floor((p.damp - p.t) * SR) : Infinity;
    for (let i = 0; i < tone.length && off + i < x.length; i++) {
      let g = p.amp;
      if (i > dEnd) g *= Math.max(0, 1 - (i - dEnd) / (0.03 * SR));
      x[off + i] += tone[i] * g;
    }
    const nb = Math.floor(0.006 * SR);          // estalo da palhetada
    for (let i = 0; i < nb && off + i < x.length; i++) x[off + i] += (r() * 2 - 1) * p.amp * 0.5 * (1 - i / nb);
  }
  for (let i = 0; i < x.length; i++) x[i] += (r() * 2 - 1) * o.noise * 1.732;
  if (o.phone) { filt(biquad('hp', 220, 0.707, SR), x); }        // microfone de celular: grave fraco
  filt(biquad('hp', 62, 0.707, SR), x); filt(biquad('lp', 2600, 0.707, SR), x); filt(biquad('lp', 2600, 0.707, SR), x);
  return x;
}

function run(x, schedule) {
  // schedule: [{t, targets:[midi]}] -> avisa worklet e rastreador nesse instante
  const D = Math.max(1, Math.round(SR / 24000)), fsd = SR / D, W = 2 * Math.round(fsd * 0.0427 / 2), hop = Math.round(fsd * 0.0107);
  const ctx = { sampleRate: SR, currentTime: 0, out: [], console };
  ctx.AudioWorkletProcessor = class { constructor() { this.port = { postMessage: m => ctx.out.push(m), onmessage: null }; } };
  ctx.registerProcessor = (n, c) => { ctx.cls = c; };
  vm.createContext(ctx); vm.runInContext(workletSource(), ctx);
  const proc = new ctx.cls({ processorOptions: { D, W, hop } });
  const evs = [], tr = makeTracker(e => evs.push(e));
  let si = 0, tid = 0, cost = 0, frames = 0;
  for (let b = 0; (b + 1) * 128 <= x.length; b++) {
    ctx.currentTime = b * 128 / SR;
    while (schedule && si < schedule.length && schedule[si].t <= ctx.currentTime) {
      tid++; proc.port.onmessage({ data: { targets: schedule[si].targets.map(mtof), tid } });
      tr.setTargets(schedule[si].targets, tid, ctx.currentTime); si++;
    }
    const t0 = process.hrtime.bigint();
    proc.process([[x.subarray(b * 128, (b + 1) * 128)]]);
    cost += Number(process.hrtime.bigint() - t0);
    for (const m of ctx.out) { frames++; tr.push(m); }
    ctx.out.length = 0;
  }
  return { evs, usPerFrame: cost / 1000 / Math.max(1, frames), W, hop, fsd };
}

function score(plucks, evs, label) {
  let ok = 0, lat = [], wrong = 0, miss = [];
  const notas = evs.filter(e => e.type === 'nota');
  plucks.forEach((p, i) => {
    const end = (plucks[i + 1] ? plucks[i + 1].t : p.t + 0.6);
    const w = notas.filter(e => e.t >= p.t - 0.005 && e.t < end);
    const hit = w.find(e => e.midi === p.midi);
    if (hit) { ok++; lat.push((hit.t - p.t) * 1000); } else miss.push(p.midi + (w.length ? '->' + w.map(e => e.midi + (e.ataque ? '' : '*')).join(',') : ''));
    wrong += w.filter(e => e.midi !== p.midi && e.ataque && (!hit || e.t < hit.t + 0.25)).length;
  });
  lat.sort((a, b) => a - b);
  const med = lat.length ? lat[lat.length >> 1].toFixed(0) : '-', p90 = lat.length ? lat[Math.floor(lat.length * 0.9)].toFixed(0) : '-';
  console.log(`${label.padEnd(46)} acertos ${String(ok).padStart(3)}/${plucks.length}  lat med ${med} ms  p90 ${p90} ms  falsos ${wrong}${miss.length ? '  perdidas: ' + miss.slice(0, 8).join(' ') : ''}`);
  return { ok, n: plucks.length, wrong };
}

const all = [];
function caso(label, plucks, dur, o, sched) {
  const x = render(plucks, dur, o), r = run(x, sched);
  all.push(score(plucks, r.evs, label));
  return r;
}

// A) notas isoladas, abafadas antes da próxima — Mi2 (40) até Mi5 (76)
for (const phone of [false, true]) {
  const pl = []; let t = 0.6;
  for (let m = 40; m <= 76; m++) { pl.push({ t, midi: m, amp: 0.2, damp: t + 0.45 }); t += 0.6; }
  const r = caso('A isoladas' + (phone ? ' (mic de celular)' : ''), pl, t + 0.5, { phone });
  if (!phone) console.log(`   janela ${r.W} amostras @ ${r.fsd} Hz, passo ${r.hop}; custo ${r.usPerFrame.toFixed(0)} µs por quadro`);
}

// B) notas que continuam soando (cordas soltas / sem abafar), sem alvo (só MPM)
const esc = [40, 43, 45, 47, 50, 52, 55, 57, 59, 62, 64, 67, 64, 62, 59, 57, 55, 52, 50, 47, 45, 43, 40];
for (const phone of [false, true]) {
  const pl = []; let t = 0.6;
  for (const m of esc) { pl.push({ t, midi: m, amp: 0.2 }); t += 0.5; }
  caso('B soando, sem alvo' + (phone ? ' (celular)' : ''), pl, t + 0.5, { phone });
}

// C) idem, com o alvo avisado 300 ms antes (modo guiado)
for (const phone of [false, true]) {
  for (const gap of [0.5, 0.3]) {
    const pl = [], sc = []; let t = 0.6;
    for (const m of esc) { pl.push({ t, midi: m, amp: 0.2 }); sc.push({ t: t - gap * 0.6, targets: [m] }); t += gap; }
    caso(`C soando, com alvo, 1 nota a cada ${gap}s` + (phone ? ' (cel)' : ''), pl, t + 0.5, { phone }, sc);
  }
}

// D) nota nova mais fraca que a que está soando
{
  const pl = [], sc = []; let t = 0.6;
  for (const m of esc) { pl.push({ t, midi: m, amp: pl.length % 2 ? 0.1 : 0.25 }); sc.push({ t: t - 0.3, targets: [m] }); t += 0.5; }
  caso('D soando, com alvo, forte/fraco alternado', pl, t + 0.5, {}, sc);
}

// E) alvo errado de propósito: o jogador toca outra nota -> não pode dar acerto
{
  const pl = [], sc = []; let t = 0.6;
  for (const m of esc) { pl.push({ t, midi: m, amp: 0.2 }); sc.push({ t: t - 0.3, targets: [m + 2] }); t += 0.5; }
  const x = render(pl, t + 0.5, {}), r = run(x, sc);
  const falsos = r.evs.filter(e => e.type === 'nota' && e.via === 'alvo').length;
  console.log(`E alvo = nota vizinha (2 semitons acima)        acertos falsos pelo caminho alvo: ${falsos}`);
  all.push({ ok: falsos === 0 ? 1 : 0, n: 1, wrong: falsos });
}

// F) alvo uma oitava acima / abaixo do que é tocado
for (const d of [12, -12]) {
  const pl = [], sc = []; let t = 0.6;
  for (const m of [45, 48, 50, 52, 55, 57, 60, 62, 64]) { pl.push({ t, midi: m, amp: 0.2, damp: t + 0.4 }); sc.push({ t: t - 0.3, targets: [m + d] }); t += 0.5; }
  for (const phone of [false, true]) {
    const x = render(pl, t + 0.5, { phone }), r = run(x, sc);
    const falsos = r.evs.filter(e => e.type === 'nota' && e.via === 'alvo').length;
    console.log(`F alvo ${d > 0 ? 'oitava acima ' : 'oitava abaixo'}${phone ? ' (cel)' : '      '}                  acertos falsos: ${falsos}/9`);
  }
}

// G) silêncio com ruído e batidas (sem nota): nenhum evento de nota
{
  const r0 = rng(3), x = new Float32Array(SR * 4);
  for (let i = 0; i < x.length; i++) x[i] = (r0() * 2 - 1) * 0.004;
  for (const tt of [1, 1.7, 2.6]) for (let i = 0; i < 0.02 * SR; i++) x[Math.floor(tt * SR) + i] += (r0() * 2 - 1) * 0.3 * (1 - i / (0.02 * SR));
  const r = run(x, [{ t: 0.2, targets: [57] }]);
  const n = r.evs.filter(e => e.type === 'nota').length;
  console.log(`G ruído + batidas, sem nota                      eventos de nota: ${n}`);
  all.push({ ok: n === 0 ? 1 : 0, n: 1, wrong: n });
}

// H) tom de uma "gravação": progressões sintéticas
function prog(chords, sr) {
  const x = new Float32Array(sr * chords.length * 2);
  chords.forEach((ch, ci) => ch.forEach((m, k) => {
    for (let h = 1; h <= 5; h++) {
      const f = mtof(m) * h, a = 0.15 / h / (k === 0 ? 0.7 : 1);
      for (let i = 0; i < sr * 2; i++) x[ci * 2 * sr + i] += a * Math.sin(2 * Math.PI * f * i / sr) * Math.exp(-i / sr);
    }
  }));
  return x;
}
const NOMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const tri = (root, minor) => [root - 12, root, root + (minor ? 3 : 4), root + 7];
[
  ['C–G–Am–F (Dó maior)', [tri(48, 0), tri(55, 0), tri(57, 1), tri(53, 0)], [0, 9]],
  ['Am–F–C–G (Lá menor)', [tri(57, 1), tri(53, 0), tri(48, 0), tri(55, 0)], [9, 0]],
  ['Em–C–G–D (Mi menor)', [tri(52, 1), tri(48, 0), tri(55, 0), tri(50, 0)], [4, 7]],
  ['D–A–Bm–G (Ré maior)', [tri(50, 0), tri(57, 0), tri(59, 1), tri(55, 0)], [2, 11]]
].forEach(([nome, chs, aceitos]) => {
  const k = detectKey(prog([...chs, ...chs, ...chs], 44100), 44100);
  const ok = aceitos.includes(k.tonica);
  console.log(`H tom de ${nome.padEnd(24)} -> ${NOMES[k.tonica]} ${k.menor ? 'menor' : 'maior'} (conf ${k.confianca.toFixed(2)}) ${ok ? 'ok (tom ou relativo)' : 'ERRADO'}`);
  all.push({ ok: ok ? 1 : 0, n: 1, wrong: ok ? 0 : 1 });
});

const tot = all.reduce((a, r) => ({ ok: a.ok + r.ok, n: a.n + r.n, wrong: a.wrong + r.wrong }), { ok: 0, n: 0, wrong: 0 });
console.log(`\nTOTAL: ${tot.ok}/${tot.n} acertos, ${tot.wrong} falsos`);
