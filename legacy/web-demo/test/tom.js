// Bancada do estimador de tom: "músicas" sintéticas (baixo + acordes de violão + melodia) em 24 tons.
const fs = require('fs'), vm = require('vm'), path = require('path');
const sb = { console };
vm.createContext(sb);
vm.runInContext(fs.readFileSync(path.join(__dirname, '../src/dsp.js'), 'utf8') + '\nthis.API = { detectKey, ksPluck };', sb);
const { detectKey, ksPluck } = sb.API;
const SR = 22050, mtof = m => 440 * Math.pow(2, (m - 69) / 12);
function rng(a) { return function () { a |= 0; a = a + 0x6D2B79F5 | 0; let t = Math.imul(a ^ a >>> 15, 1 | a); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; }; }
const cache = new Map();
function tone(m, r) { if (!cache.has(m)) cache.set(m, ksPluck(SR, mtof(m), 1.6, r)); return cache.get(m); }
const MAJ = [0, 2, 4, 5, 7, 9, 11], MIN = [0, 2, 3, 5, 7, 8, 10];
// progressões em graus da escala (índice 0..6) da própria tonalidade
const PROGS = { maior: [[0, 4, 5, 3], [0, 3, 4, 3], [0, 5, 3, 4], [1, 4, 0, 0], [0, 4, 3, 3]], menor: [[0, 5, 2, 6], [0, 3, 4, 0], [0, 6, 5, 6], [0, 3, 6, 2], [0, 5, 3, 4]] };
function song(tonic, menor, prog, seed, o) {
  const r = rng(seed), esc = menor ? MIN : MAJ, bars = prog.length * 3, x = new Float32Array(SR * 2 * bars + SR);
  const add = (m, t, a) => { const d = tone(m, r), off = Math.floor(t * SR); for (let i = 0; i < d.length && off + i < x.length; i++) x[off + i] += d[i] * a; };
  for (let b = 0; b < bars; b++) {
    const g = prog[b % prog.length], pcs = [0, 2, 4].map(k => (tonic + esc[(g + k) % 7] + (g + k >= 7 ? 0 : 0)) % 12), t0 = b * 2;
    const root = pcs[0];
    if (o.baixo) { add(36 + root, t0, 0.35); add(36 + root, t0 + 1, 0.3); }
    // acorde: 5 notas entre 48 e 72 com as classes da tríade, duas "batidas"
    const voz = []; for (let m = 48; m <= 71 && voz.length < 5; m++) if (pcs.indexOf(m % 12) >= 0 && r() < 0.75) voz.push(m);
    for (const beat of [0, 1]) voz.forEach((m, j) => add(m, t0 + beat + j * 0.02, 0.14));
    if (o.melodia) for (let q = 0; q < 4; q++) { const deg = r() < 0.45 ? [0, 2, 4][(r() * 3) | 0] + g : (r() * 7) | 0; add(60 + ((tonic + esc[deg % 7]) % 12), t0 + q * 0.5, 0.16); }
  }
  for (let i = 0; i < x.length; i++) x[i] += (r() * 2 - 1) * 0.003;
  return x;
}
function avalia(nome, opt, o) {
  const cnt = { exato: 0, relativo: 0, quinta: 0, paralelo: 0, outro: 0 }; let n = 0;
  for (const menor of [false, true]) for (let tonic = 0; tonic < 12; tonic++) PROGS[menor ? 'menor' : 'maior'].forEach((prog, pi) => {
    const k = detectKey(song(tonic, menor, prog, 100 + tonic * 7 + pi, o), SR, opt); n++;
    const rel = menor ? (tonic + 3) % 12 : (tonic + 9) % 12;
    if (k.tonica === tonic && k.menor === menor) cnt.exato++;
    else if (k.tonica === rel && k.menor !== menor) cnt.relativo++;
    else if (k.tonica === tonic && k.menor !== menor) cnt.paralelo++;
    else if ((k.tonica === (tonic + 7) % 12 || k.tonica === (tonic + 5) % 12)) cnt.quinta++;
    else if (k.menor !== menor && (k.tonica === (rel + 7) % 12 || k.tonica === (rel + 5) % 12)) cnt.quinta++;
    else cnt.outro++;
  });
  const boa = cnt.exato + cnt.relativo;
  console.log(`${nome.padEnd(34)} mesmo conjunto de notas ${String(boa).padStart(3)}/${n} (exato ${cnt.exato}, relativo ${cnt.relativo}) | vizinho de quinta ${cnt.quinta} | paralelo ${cnt.paralelo} | outro ${cnt.outro}`);
}
for (const o of [{ baixo: true, melodia: true, tag: 'baixo+melodia' }, { baixo: false, melodia: false, tag: 'só acordes' }]) {
  console.log('--', o.tag);
  for (const perfil of ['kk', 'temperley', 'shaath']) for (const explica of [false, true]) avalia(perfil + (explica ? ' + explica-harmônico' : ''), { perfil, explica }, o);
}
