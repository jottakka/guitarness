/* =====================================================================
   Núcleo da interface: utilidades, progresso salvo, efeitos e o braço
   ===================================================================== */
const $ = (s, r) => (r || document).querySelector(s);
const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));
const clamp = (v, a, b) => (v < a ? a : v > b ? b : v);
const agora = () => performance.now();
const SVGNS = 'http://www.w3.org/2000/svg';
function el(tag, cls, txt) { const e = document.createElement(tag); if (cls) e.className = cls; if (txt != null) e.textContent = txt; return e; }
function svg(tag, attrs) { const e = document.createElementNS(SVGNS, tag); for (const k in attrs) e.setAttribute(k, attrs[k]); return e; }
function hojeStr() { const d = new Date(); return d.getFullYear() + '-' + String(d.getMonth() + 1).padStart(2, '0') + '-' + String(d.getDate()).padStart(2, '0'); }
function embaralha(a) { a = a.slice(); for (let i = a.length - 1; i > 0; i--) { const j = (Math.random() * (i + 1)) | 0; [a[i], a[j]] = [a[j], a[i]]; } return a; }
function nomeNota(pc, bemol) { return pt(pc, bemol) + ' (' + abc(pc, bemol) + ')'; }

/* ---------- progresso (fica só neste navegador) ---------- */
const Store = (() => {
  const KEY = 'tonica.v1';
  let d = null;
  const base = () => ({ xp: 0, dias: [], hoje: { d: '', n: 0 }, best: {}, pool: {}, notas: {}, afinou: '', cfg: { fone: false } });
  function get() {
    if (d) return d;
    d = base();
    try { const raw = localStorage.getItem(KEY); if (raw) Object.assign(d, JSON.parse(raw)); } catch (e) { /* sem armazenamento: segue em memória */ }
    if (!d.cfg) d.cfg = { fone: false };
    if (!d.hoje) d.hoje = { d: '', n: 0 };
    return d;
  }
  function save() { try { localStorage.setItem(KEY, JSON.stringify(d)); } catch (e) { /* idem */ } }
  return { get, save };
})();

const TITULOS = ['Primeira corda', 'Dedo quente', 'Caçador de notas', 'Mão de escala', 'Arquiteto de acordes', 'Solista de quarto', 'Lenda da roda'];
const xpDoNivel = n => 150 * n * n;                       // XP acumulado para passar do nível n
function nivelDe(xp) { let n = 1; while (xp >= xpDoNivel(n)) n++; return n; }
function tituloDe(n) { return TITULOS[Math.min(TITULOS.length, n) - 1]; }
function rodadasHoje() { const s = Store.get(); return s.hoje.d === hojeStr() ? s.hoje.n : 0; }

function pintarTopo() {
  const s = Store.get(), n = nivelDe(s.xp), a = n > 1 ? xpDoNivel(n - 1) : 0, b = xpDoNivel(n);
  $('#xpTxt').textContent = 'Nível ' + n + ' · ' + fmt(s.xp) + ' XP';
  $('#xpTxt').title = tituloDe(n);
  $('#xpFill').style.width = clamp((s.xp - a) / (b - a) * 100, 0, 100) + '%';
}
function ganharXP(q) {
  const s = Store.get(), antes = nivelDe(s.xp);
  s.xp += Math.max(0, Math.round(q));
  Store.save(); pintarTopo();
  const depois = nivelDe(s.xp);
  if (depois > antes) { toast('Nível ' + depois + ': ' + tituloDe(depois)); FX.confete(); }
}
function contarRodada() {
  const s = Store.get(), h = hojeStr();
  if (s.hoje.d !== h) s.hoje = { d: h, n: 0 };
  s.hoje.n++;
  if (s.dias.indexOf(h) < 0) s.dias.push(h);
  if (s.dias.length > 500) s.dias.shift();
  Store.save();
  if (s.hoje.n === 3) { toast('Meta de hoje fechada'); FX.confete(); }
}

/* ---------- efeitos ---------- */
const FX = (() => {
  const cv = $('#fx'), cx = cv.getContext('2d');
  const calmo = !!(window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches);
  const ps = [];
  let W = 0, H = 0, sujo = false;
  function resize() {
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    W = window.innerWidth; H = window.innerHeight;
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    cx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  window.addEventListener('resize', resize); resize();
  function estouro(x, y, cor, n, forca) {
    n = calmo ? 5 : (n || 18); forca = forca || 1;
    for (let i = 0; i < n; i++) {
      const a = Math.random() * 6.283, v = (80 + Math.random() * 260) * forca;
      ps.push({ x, y, vx: Math.cos(a) * v, vy: Math.sin(a) * v - 70, g: 560, r: 2.5 + Math.random() * 4, cor, vida: 0.45 + Math.random() * 0.45, t: 0, tipo: 0 });
    }
    ps.push({ x, y, vx: 0, vy: 0, g: 0, r: 14, cor, vida: 0.4, t: 0, tipo: 1 });
  }
  function confete() {
    if (calmo) return;
    for (let i = 0; i < 90; i++) ps.push({ x: Math.random() * W, y: -10 - Math.random() * 120, vx: (Math.random() - 0.5) * 140, vy: 140 + Math.random() * 240, g: 180, r: 3 + Math.random() * 4, cor: IV_COR[(Math.random() * 12) | 0], vida: 1.7 + Math.random(), t: 0, tipo: 2 });
  }
  function quadro(dt) {
    if (!ps.length) { if (sujo) { cx.clearRect(0, 0, W, H); sujo = false; } return; }
    cx.clearRect(0, 0, W, H); sujo = true;
    for (let i = ps.length - 1; i >= 0; i--) {
      const p = ps[i]; p.t += dt;
      if (p.t >= p.vida) { ps.splice(i, 1); continue; }
      const k = 1 - p.t / p.vida;
      if (p.tipo === 1) {
        cx.globalAlpha = k; cx.strokeStyle = p.cor; cx.lineWidth = 1 + 4 * k;
        cx.beginPath(); cx.arc(p.x, p.y, p.r + (1 - k) * 50, 0, 6.283); cx.stroke();
      } else {
        p.vy += p.g * dt; p.x += p.vx * dt; p.y += p.vy * dt;
        cx.globalAlpha = Math.min(1, k * 1.8); cx.fillStyle = p.cor;
        if (p.tipo === 2) cx.fillRect(p.x, p.y, p.r * 1.7, p.r);
        else { cx.beginPath(); cx.arc(p.x, p.y, p.r * (0.45 + 0.55 * k), 0, 6.283); cx.fill(); }
      }
    }
    cx.globalAlpha = 1;
  }
  return { estouro, confete, quadro };
})();

function flutua(x, y, txt, cor) {
  const e = el('div', 'flutua' + (txt.length > 5 ? ' peq' : ''), txt), m = 12 + txt.length * 5;
  e.style.left = clamp(x, m, window.innerWidth - m) + 'px'; e.style.top = Math.max(70, y) + 'px'; if (cor) e.style.color = cor;
  document.body.appendChild(e); setTimeout(() => e.remove(), 900);
}
function toast(txt) {
  const t = el('div', 'toast', txt);
  $('#toasts').appendChild(t);
  setTimeout(() => t.classList.add('sai'), 2300); setTimeout(() => t.remove(), 2800);
}
function bump(e) { e.classList.remove('bump'); void e.offsetWidth; e.classList.add('bump'); }
function fala(e, txt, tom) { e.textContent = txt; e.className = 'fala' + (tom ? ' ' + tom : ''); }

/* ---------- braço do violão (SVG) ----------
   cfg: { de, ate, aoTocar(midi, corda, casa) }. Corda 0 = 6ª (Mi grave), desenhada embaixo,
   como numa tablatura. */
function Braco(host, cfg) {
  const de = cfg.de, ate = cfg.ate, solta = de === 0;
  const casas = []; for (let f = Math.max(1, de); f <= ate; f++) casas.push(f);
  const VW = 430, pL = 30, pR = 10, pT = 14, RH = 38, oW = solta ? 54 : 0;
  const VH = pT + 28 + RH * 5 + 28, x0 = pL + oW, fw = (VW - pL - pR - oW) / casas.length;
  const X = f => (f === 0 ? pL + oW / 2 : x0 + (f - casas[0] + 0.5) * fw);
  const Y = s => pT + 14 + (5 - s) * RH;
  const frets = (solta ? [0] : []).concat(casas);
  const root = svg('svg', { viewBox: '0 0 ' + VW + ' ' + VH, class: 'braco', role: 'group', 'aria-label': 'Braço do violão, casas ' + de + ' a ' + ate });

  root.appendChild(svg('rect', { x: x0, y: pT, width: casas.length * fw, height: RH * 5 + 28, rx: 6, class: 'madeira' }));
  casas.forEach(f => {
    if ([3, 5, 7, 9, 15, 17].indexOf(f) >= 0) root.appendChild(svg('circle', { cx: X(f), cy: pT + 14 + RH * 2.5, r: 8, class: 'inlay' }));
    if (f === 12) { root.appendChild(svg('circle', { cx: X(f), cy: pT + 14 + RH * 1.5, r: 8, class: 'inlay' })); root.appendChild(svg('circle', { cx: X(f), cy: pT + 14 + RH * 3.5, r: 8, class: 'inlay' })); }
  });
  for (let i = 0; i <= casas.length; i++) {
    const x = x0 + i * fw;
    root.appendChild(svg('line', { x1: x, x2: x, y1: pT + 2, y2: pT + RH * 5 + 26, class: i === 0 && casas[0] === 1 ? 'pestana' : 'traste' }));
  }
  for (let s = 0; s < 6; s++) {
    root.appendChild(svg('line', { x1: solta ? pL + 4 : x0, x2: VW - pR, y1: Y(s), y2: Y(s), class: 'corda', 'stroke-width': (3.6 - s * 0.45).toFixed(2) }));
    const t = svg('text', { x: 12, y: Y(s) + 4.5, class: 'nomecorda' }); t.textContent = ['E', 'A', 'D', 'G', 'B', 'e'][s]; root.appendChild(t);
  }
  frets.forEach(f => { const t = svg('text', { x: X(f), y: VH - 8, class: 'numcasa' }); t.textContent = f === 0 ? 'solta' : f; root.appendChild(t); });

  const gDots = svg('g', {}), gHit = svg('g', {});
  root.appendChild(gDots); root.appendChild(gHit);
  for (let s = 0; s < 6; s++) frets.forEach(f => {
    const c = svg('circle', { cx: X(f), cy: Y(s), r: 18.5, class: 'toque' });
    c.addEventListener('pointerdown', ev => { ev.preventDefault(); if (cfg.aoTocar) cfg.aoTocar(CORDAS[s] + f, s, f); });
    gHit.appendChild(c);
  });
  host.textContent = ''; host.appendChild(root);

  const mapa = new Map();
  function pontos(lista) {                              // [{s, f, rot, cor, cls}]
    gDots.textContent = ''; mapa.clear();
    lista.forEach(d => {
      const g = svg('g', { class: 'ponto ' + (d.cls || ''), transform: 'translate(' + X(d.f) + ',' + Y(d.s) + ')', 'data-midi': CORDAS[d.s] + d.f });
      g.style.setProperty('--c', d.cor || '#9a93a8');
      const anel = svg('circle', { r: 21, class: 'anel', transform: 'rotate(-90)' });
      const bola = svg('circle', { r: 14.5, class: 'bola' });
      const rot = svg('text', { y: 4.6, class: 'rot' }); rot.textContent = d.rot || '';
      g.appendChild(anel); g.appendChild(bola); g.appendChild(rot); gDots.appendChild(g);
      mapa.set(d.s + ':' + d.f, { g, anel, rot, d });
    });
  }
  function ponto(s, f) { return mapa.get(s + ':' + f); }
  function centro(s, f) { const r = root.getBoundingClientRect(); return { x: r.left + X(f) / VW * r.width, y: r.top + Y(s) / VH * r.height }; }
  function onde(midi) {                                 // onde essa nota cai dentro da janela
    for (const v of mapa.values()) if (CORDAS[v.d.s] + v.d.f === midi && !/trancado/.test(v.d.cls || '')) return { s: v.d.s, f: v.d.f };
    for (let s = 5; s >= 0; s--) { const f = midi - CORDAS[s]; if (frets.indexOf(f) >= 0) return { s, f }; }
    return null;
  }
  function pisca(s, f, rot) {                           // marca passageira: "você tocou aqui"
    const g = svg('g', { class: 'ponto fantasma', transform: 'translate(' + X(f) + ',' + Y(s) + ')' });
    const bola = svg('circle', { r: 14.5, class: 'bola' }), t = svg('text', { y: 4.6, class: 'rot' }); t.textContent = rot || '';
    g.appendChild(bola); g.appendChild(t); gDots.appendChild(g);
    setTimeout(() => g.remove(), 820);
  }
  function acende(s, f) {
    const p = ponto(s, f); if (!p) return;
    p.g.classList.remove('acerto'); void p.g.getBoundingClientRect(); p.g.classList.add('acerto');
  }
  return { pontos, ponto, centro, onde, pisca, acende };
}

/* ---------- telas ---------- */
const Telas = {};
let telaAtual = null;
function ir(nome, arg) {
  if (telaAtual && telaAtual.sair) telaAtual.sair();
  $$('.tela').forEach(s => s.classList.toggle('on', s.id === 't-' + nome));
  telaAtual = Telas[nome] || null;
  window.scrollTo(0, 0);
  if (telaAtual && telaAtual.entrar) telaAtual.entrar(arg);
}
const fmt = n => Math.round(n).toLocaleString('pt-BR');
function centroDe(e) { const r = e.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; }
