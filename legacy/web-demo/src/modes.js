/* =====================================================================
   Telas e modos de jogo
   ===================================================================== */
function acendeUmPouco(braco, s, f, ms) {
  const p = braco.ponto(s, f); if (!p) return;
  p.g.classList.add('aceso'); braco.acende(s, f);
  setTimeout(() => p.g.classList.remove('aceso'), ms || 450);
}

/* ---------------- INÍCIO: braço livre + resumo do dia ---------------- */
Telas.home = (() => {
  const fase = FASES[1], notas = notasDaFase(fase), conj = notas.map(n => n.pc);
  let braco = null;
  function entrar() {
    braco = Braco($('#heroBraco'), { de: 0, ate: 3, aoTocar: m => Entrada.toque(m) });
    braco.pontos(notas.map(n => ({ s: n.s, f: n.f, rot: abc(n.pc), cor: IV_COR[n.iv], cls: n.iv === 0 ? 'tonica' : '' })));
    Entrada.alvos([]);
    const s = Store.get(), n = rodadasHoje();
    $$('#pips i').forEach((e, i) => e.classList.toggle('on', i < n));
    $('#diasN').textContent = s.dias.length; $('#rodN').textContent = n;
    let est = 0, ch = 0;
    Object.keys(s.best).forEach(k => { if (k.indexOf('hunt:') === 0) est += s.best[k].e || 0; if (k.indexOf('chord:') === 0) ch = Math.max(ch, s.best[k].p || 0); });
    $('#bHunt').textContent = est ? '★ ' + est : '';
    $('#bChord').textContent = ch ? 'recorde ' + fmt(ch) : '';
    $('#bJam').textContent = s.best.jam ? 'recorde ' + fmt(s.best.jam.p) : '';
    $('#bAfina').textContent = s.afinou === hojeStr() ? 'afinado hoje' : '';
  }
  function nota(ev) {
    const pc = ev.midi % 12, iv = (pc - fase.tonica + 12) % 12, dentro = conj.indexOf(pc) >= 0, p = braco.onde(ev.midi);
    if (p) {
      const c = braco.centro(p.s, p.f);
      if (dentro) { FX.estouro(c.x, c.y, IV_COR[iv], 16); acendeUmPouco(braco, p.s, p.f, 500); }
      else braco.pisca(p.s, p.f, abc(pc));
    }
    fala($('#heroFala'), nomeNota(pc) + ': ' + IV_NOME[iv] + ' (' + IV[iv] + ') de Mi' + (dentro ? '.' : ', fora da pentatônica.'));
  }
  return { entrar, nota };
})();

/* ---------------- CAÇA-NOTAS ---------------- */
Telas.hunt = (() => {
  let H = null, braco = null, faseSel = 1, estilo = 'guiado';
  const DICA = {
    guiado: 'A posição acende no braço. Use para aprender o desenho da escala.',
    notas: 'Aparece só o nome da nota e você acha onde ela fica. Vale qualquer oitava.',
    intervalos: 'Aparece só o intervalo em relação à tônica (T, 3m, 4J, 5J, 7m). Vale qualquer oitava.'
  };

  function pintarPrep() {
    const s = Store.get(), box = $('#huntFases'); box.textContent = '';
    FASES.forEach((f, i) => {
      const n = f.pos.length, lib = clamp(s.pool[f.id] || Math.min(4, n), 0, n);
      let e = 0; ['guiado', 'notas', 'intervalos'].forEach(k => { const b = s.best['hunt:' + f.id + ':' + k]; if (b) e = Math.max(e, b.e || 0); });
      const b = el('button', 'chip' + (i === faseSel ? ' on' : '')); b.type = 'button';
      b.appendChild(document.createTextNode(f.nome));
      const sm = el('small', '', lib + ' de ' + n + ' notas liberadas');
      if (e) sm.appendChild(el('span', 'st', ' ' + '★'.repeat(e)));
      b.appendChild(sm);
      b.addEventListener('click', () => { faseSel = i; pintarPrep(); });
      box.appendChild(b);
    });
    $$('#huntEstilo button').forEach(b => b.classList.toggle('on', b.dataset.v === estilo));
    $('#huntEstiloDica').textContent = DICA[estilo];
    const f = FASES[faseSel];
    $('#huntFaseDica').textContent = f.dica;
    braco = Braco($('#huntPrevia'), { de: f.de, ate: f.ate, aoTocar: m => Entrada.toque(m) });
    braco.pontos(notasDaFase(f).map(n => ({ s: n.s, f: n.f, rot: estilo === 'intervalos' ? IV[n.iv] : abc(n.pc), cor: IV_COR[n.iv], cls: n.iv === 0 ? 'tonica' : '' })));
  }

  function entrar(arg) {
    H = null; Entrada.alvos([]);
    $('#huntPrep').hidden = false; $('#huntJogo').hidden = true;
    pintarPrep();
    if (arg && arg.direto) comecar();
  }
  function sair() { if (H) { clearTimeout(H.pend); clearTimeout(H.prox); } H = null; Entrada.alvos([]); }

  function comecar() {
    try { Som.garante(); } catch (e) { /* segue sem som */ }
    const fase = FASES[faseSel], notas = notasDaFase(fase), st = Store.get(), min = Math.min(4, notas.length);
    H = {
      fase, estilo, notas, pool: clamp(st.pool[fase.id] || min, min, notas.length), alvo: null, limite: 6000,
      pontos: 0, combo: 0, maxCombo: 0, acertos: 0, erros: 0, expirados: 0, rts: [], seguidos: 0, pend: 0, prox: 0,
      estado: 'contagem', contaAte: agora() + 2400, fimEm: 0, novo: null
    };
    $('#huntPrep').hidden = true; $('#huntJogo').hidden = false;
    braco = Braco($('#huntBraco'), { de: fase.de, ate: fase.ate, aoTocar: m => Entrada.toque(m) });
    pintarBraco(); pintarHud();
    $('#hTempo').textContent = '60';
    $('#hSub').textContent = 'Prepare o violão';
    $('#hPedido').style.removeProperty('--c'); $('#hBarra').style.transform = 'scaleX(1)';
    fala($('#hFala'), Som.ligado() ? 'Microfone ligado: toque no violão.' : 'Modo toque: toque nas bolinhas do braço.');
    Entrada.alvos([]);
    window.scrollTo(0, 0);
  }

  function pintarHud() {
    $('#hPontos').textContent = fmt(H.pontos);
    $('#hCombo').textContent = H.combo;
  }

  function pintarBraco() {
    const a = H.alvo;
    braco.pontos(H.notas.map((n, i) => {
      const livre = i < H.pool;
      const ehAlvo = !!a && livre && (H.estilo === 'guiado' ? n.midi === a.midi : a.revelado ? n.pc === a.pc : a.guia ? n.midi === a.midi : false);
      let cls = '';
      if (!livre) cls = 'trancado';
      else if (ehAlvo) cls = 'alvo';
      else if (H.estilo !== 'guiado' && n.iv !== 0) cls = 'oculto';
      if (livre && n.iv === 0) cls += ' tonica';
      return { s: n.s, f: n.f, rot: H.estilo === 'intervalos' ? IV[n.iv] : abc(n.pc), cor: IV_COR[n.iv], cls };
    }));
  }

  function pintarPedido() {
    const a = H.alvo, f = H.fase;
    $('#hPedido').style.setProperty('--c', IV_COR[a.iv]);
    if (H.estilo === 'intervalos') { $('#hNota').textContent = IV[a.iv]; $('#hSub').textContent = IV_NOME[a.iv] + ' de ' + pt(f.tonica); }
    else if (H.estilo === 'notas') { $('#hNota').textContent = pt(a.pc); $('#hSub').textContent = abc(a.pc) + ' · ache no braço'; }
    else { $('#hNota').textContent = pt(a.pc); $('#hSub').textContent = abc(a.pc) + ' · ' + IV[a.iv] + ' · ' + CORDA_NOME[a.s] + ' corda, ' + (a.f === 0 ? 'solta' : 'casa ' + a.f); }
  }

  function proximo(forcado) {
    const s = Store.get(), ant = H.ultimo;
    let cand = H.notas.slice(0, H.pool);
    if (ant) { const c2 = cand.filter(n => (H.estilo === 'guiado' ? n.midi !== ant.midi : n.pc !== ant.pc)); if (c2.length) cand = c2; }
    let esc = forcado || null;
    if (!esc) {
      // notas mais lentas ou mais erradas aparecem mais
      const pesos = cand.map(n => { const e = s.notas[H.fase.id + ':' + n.midi]; return !e || !e.ok ? 2.2 : clamp(e.rt / 1800, 0.5, 2.5) + (e.er || 0) / (e.ok + 1); });
      let r = Math.random() * pesos.reduce((x, y) => x + y, 0);
      esc = cand[cand.length - 1];
      for (let i = 0; i < cand.length; i++) { r -= pesos[i]; if (r <= 0) { esc = cand[i]; break; } }
    }
    H.alvo = Object.assign({}, esc, { em: agora(), limite: H.limite, revelado: false, guia: !!forcado && H.estilo !== 'guiado', erros: 0 });
    H.ultimo = esc;
    let ms = [esc.midi];
    if (H.estilo !== 'guiado') { ms = []; for (let m = 40; m <= 79; m++) if (m % 12 === esc.pc) ms.push(m); }
    Entrada.alvos(ms);
    pintarPedido(); pintarBraco();
  }

  function registra(a, ok, rt) {
    const s = Store.get(), k = H.fase.id + ':' + a.midi, e = s.notas[k] || { ok: 0, er: 0, rt: 2500 };
    if (ok) { e.rt = e.ok ? e.rt * 0.7 + rt * 0.3 : rt; e.ok++; } else e.er++;
    s.notas[k] = e;
  }

  function nota(ev) {
    if (!H) {                                                    // tela de preparo: braço livre
      const f = FASES[faseSel], pc = ev.midi % 12, p = braco && braco.onde(ev.midi);
      if (p && braco.ponto(p.s, p.f)) { const c = braco.centro(p.s, p.f); FX.estouro(c.x, c.y, IV_COR[(pc - f.tonica + 12) % 12], 12); acendeUmPouco(braco, p.s, p.f); }
      return;
    }
    if (H.estado !== 'jogo' || !H.alvo) return;
    const a = H.alvo, pc = ev.midi % 12;
    if (H.estilo === 'guiado' ? ev.midi === a.midi : pc === a.pc) { acerto(ev); return; }
    if (ev.via === 'alvo' || !ev.ataque) return;
    if (H.estilo === 'guiado' && pc === a.pc) { fala($('#hFala'), 'Nota certa, oitava diferente. Vá na bolinha que pulsa.'); return; }
    clearTimeout(H.pend);
    if (ev.via === 'toque') erro(ev); else H.pend = setTimeout(() => erro(ev), 130);   // um acerto pode chegar logo atrás
  }

  function acerto(ev) {
    clearTimeout(H.pend);
    const a = H.alvo, rt = agora() - a.em, cor = IV_COR[a.iv];
    let pts = 20;
    if (!a.revelado) {
      const mult = 1 + Math.min(3, Math.floor(H.combo / 5)) * 0.5;
      pts = Math.round((100 + 100 * clamp(1 - rt / a.limite, 0, 1)) * mult);
      H.combo++; H.seguidos++; H.acertos++; H.rts.push(rt);
      if (H.combo > H.maxCombo) H.maxCombo = H.combo;
      H.limite = Math.max(1500, H.limite * 0.94);            // acertou: um pouco mais rápido (equilíbrio perto de 85% de acerto)
      registra(a, true, rt);
    }
    H.pontos += pts;
    let p = H.estilo === 'guiado' || a.guia ? { s: a.s, f: a.f } : braco.onde(ev.midi);
    if (!p || !braco.ponto(p.s, p.f)) p = { s: a.s, f: a.f };
    const c = braco.centro(p.s, p.f), d = braco.ponto(p.s, p.f);
    FX.estouro(c.x, c.y, cor, 16 + Math.min(18, H.combo * 2), 1 + Math.min(0.5, H.combo * 0.04));
    flutua(c.x, c.y - 22, '+' + pts, cor);
    if (d) { d.g.classList.remove('alvo', 'oculto'); d.g.classList.add('aceso'); braco.acende(p.s, p.f); }
    pintarHud(); bump($('#hPontos')); if (H.combo && H.combo % 5 === 0) bump($('#hCombo'));
    const f = $('#hFala');
    if (a.revelado) fala(f, 'Isso. Agora valendo.');
    else if (H.combo > 0 && H.combo % 5 === 0 && H.combo <= 15) fala(f, H.combo + ' seguidas: pontos ×' + String(1 + Math.min(3, H.combo / 5) * 0.5).replace('.', ','), 'bom');
    else if (rt < 1300) fala(f, 'Rápido!', 'bom');
    else fala(f, nomeNota(a.pc) + ', ' + IV_NOME[a.iv] + '.');
    if (H.seguidos >= 5 && H.pool < H.notas.length) {        // cinco seguidas: libera a próxima nota
      H.pool++; H.seguidos = 0; H.novo = H.notas[H.pool - 1];
      toast('Nota nova: ' + nomeNota(H.novo.pc));
    }
    H.alvo = null;
    H.prox = setTimeout(() => { if (H && H.estado === 'jogo') { const n = H.novo; H.novo = null; proximo(n); } }, 230);
  }

  function erro(ev) {
    if (!H || H.estado !== 'jogo' || !H.alvo) return;
    const a = H.alvo, pc = ev.midi % 12, p = braco.onde(ev.midi);
    H.erros++; a.erros++; H.combo = Math.floor(H.combo / 2); H.seguidos = 0;
    registra(a, false);
    if (p) braco.pisca(p.s, p.f, abc(pc));
    pintarHud();
    fala($('#hFala'), 'Isso foi ' + nomeNota(pc) + '. ' + (H.estilo === 'guiado' ? 'O alvo é a bolinha que pulsa.' : 'Procure ' + (H.estilo === 'intervalos' ? 'a ' + IV[a.iv] + ' de ' + pt(H.fase.tonica) : pt(a.pc)) + '.'), 'ruim');
    if (H.estilo !== 'guiado' && a.erros >= 2 && !a.revelado) revelar('Olha onde fica.');
  }

  function revelar(msg) {
    const a = H.alvo; a.revelado = true;
    H.limite = Math.min(9000, H.limite * 1.4);               // precisou de ajuda: mais devagar
    $('#hBarra').style.transform = 'scaleX(0)';
    pintarBraco();
    fala($('#hFala'), msg + ' Toque ' + pt(a.pc) + ' para seguir.');
  }

  function update() {
    if (!H) return;
    const t = agora();
    if (H.estado === 'contagem') {
      const r = H.contaAte - t;
      if (r <= 0) { H.estado = 'jogo'; H.fimEm = t + 60000; proximo(); }
      else $('#hNota').textContent = Math.ceil(r / 800);
      return;
    }
    if (H.estado !== 'jogo') return;
    const resta = H.fimEm - t;
    $('#hTempo').textContent = Math.max(0, Math.ceil(resta / 1000));
    if (resta <= 0) { fim(); return; }
    const a = H.alvo;
    if (a && !a.revelado) {
      const k = clamp((t - a.em) / a.limite, 0, 1);
      $('#hBarra').style.transform = 'scaleX(' + (1 - k).toFixed(3) + ')';
      const d = braco.ponto(a.s, a.f);
      if (d && (H.estilo === 'guiado' || a.guia)) d.anel.style.strokeDashoffset = (132 * k).toFixed(1);
      if (k >= 1) {
        H.expirados++; H.combo = Math.floor(H.combo / 2); H.seguidos = 0; registra(a, false); pintarHud();
        revelar('Sem pressa.');
      }
    }
  }

  function fim() {
    const s = Store.get(), id = 'hunt:' + H.fase.id + ':' + H.estilo, h = H;
    clearTimeout(h.pend); clearTimeout(h.prox);
    h.estado = 'fim'; Entrada.alvos([]);
    const tent = h.acertos + h.erros + h.expirados, prec = tent ? h.acertos / tent : 0;
    const med = h.rts.length ? h.rts.slice().sort((x, y) => x - y)[h.rts.length >> 1] / 1000 : 0;
    const e = h.acertos >= 26 ? 3 : h.acertos >= 18 ? 2 : h.acertos >= 10 ? 1 : 0;
    const ant = s.best[id], recorde = !ant || h.pontos > ant.p;
    s.best[id] = { p: Math.max(h.pontos, ant ? ant.p : 0), e: Math.max(e, ant ? ant.e || 0 : 0) };
    const cresceu = h.pool > (s.pool[h.fase.id] || 0);
    s.pool[h.fase.id] = Math.max(h.pool, s.pool[h.fase.id] || 0);
    contarRodada(); Store.save();
    const xp = Math.round(h.pontos / 10) + 20;
    ganharXP(xp);
    const nomeEstilo = { guiado: 'Guiado', notas: 'Notas', intervalos: 'Intervalos' }[h.estilo];
    ir('fim', {
      olho: 'Caça-Notas · ' + h.fase.nome + ' · ' + nomeEstilo,
      titulo: recorde && h.pontos > 0 ? 'Novo recorde' : 'Rodada concluída',
      pontos: h.pontos, estrelas: e, festa: e >= 2 || (recorde && h.acertos >= 8),
      linhas: [
        ['Notas certas', String(h.acertos)], ['Precisão', Math.round(prec * 100) + '%'],
        ['Tempo de reação (mediana)', med ? med.toFixed(1).replace('.', ',') + ' s' : '–'], ['Maior sequência', String(h.maxCombo)],
        ['Notas liberadas', h.pool + ' de ' + h.notas.length], ['XP ganho', '+' + xp]
      ],
      msg: h.acertos < 10 ? 'A primeira estrela vem com 10 notas certas. A velocidade se ajusta: é só repetir.' :
        cresceu ? 'Você liberou nota nova nesta rodada. Ela volta com mais frequência até ficar rápida.' :
          h.estilo === 'guiado' && e >= 2 ? 'Desenho na mão. Tente o estilo Notas ou Intervalos nesta fase.' : 'Mais uma rodada fixa o que acabou de entrar.',
      deNovo: () => ir('hunt', { direto: true })
    });
  }

  $$('#huntEstilo button').forEach(b => b.addEventListener('click', () => { estilo = b.dataset.v; pintarPrep(); }));
  $('#huntGo').addEventListener('click', comecar);
  $('#huntSair').addEventListener('click', () => { if (H && H.estado === 'jogo' && H.acertos > 0) fim(); else ir('hunt'); });
  return { entrar, sair, nota, update };
})();

/* ---------------- MONTA-ACORDE ---------------- */
Telas.chord = (() => {
  let C = null, braco = null, estilo = 'guiado';
  const ORDEM = [0, 4, 5, 3, 1, 2];                              // C, G, Am, F, Dm, Em
  const DICA = {
    guiado: 'O jogo mostra as três notas e onde elas ficam no desenho do acorde. Segure o acorde e toque uma corda por vez.',
    rapido: 'Só o nome do acorde. Conte na roda: tônica, pula uma, terça, pula uma, quinta. Quanto mais rápido, maior o bônus.'
  };
  const NATURAIS = [];
  for (let s = 0; s < 6; s++) for (let f = 0; f <= 3; f++) { const pc = (CORDAS[s] + f) % 12; if (ESCALA_C.indexOf(pc) >= 0) NATURAIS.push({ s, f, pc }); }

  function pintarPrep() {
    $$('#chEstilo button').forEach(b => b.classList.toggle('on', b.dataset.v === estilo));
    $('#chDica').textContent = DICA[estilo];
  }
  function entrar() { C = null; Entrada.alvos([]); $('#chPrep').hidden = false; $('#chJogo').hidden = true; pintarPrep(); }
  function sair() { if (C) { clearTimeout(C.pend); clearTimeout(C.auto); } C = null; Entrada.alvos([]); }

  function comecar() {
    try { Som.garante(); } catch (e) { /* segue sem som */ }
    C = { i: 0, ordem: estilo === 'guiado' ? ORDEM.slice() : embaralha(ORDEM), estilo, pontos: 0, limpos: 0, estado: 'jogo', pend: 0, auto: 0 };
    $('#chPrep').hidden = true; $('#chJogo').hidden = false;
    braco = Braco($('#chBraco'), { de: 0, ate: 3, aoTocar: m => Entrada.toque(m) });
    $('#cPontos').textContent = '0';
    carregar(); window.scrollTo(0, 0);
  }

  function carregar() {
    const ac = ACORDES[C.ordem[C.i]];
    Object.assign(C, { ac, tri: triade(ac), achou: [false, false, false], dicas: [false, false, false], em: agora(), erros: 0, pronto: false, dicaTempo: false });
    $('#cProx').hidden = true;
    $('#cNum').textContent = (C.i + 1) + '/' + C.ordem.length;
    $('#cExt').textContent = ac.ext + ' · grau ' + ac.grau;
    fala($('#cFala'), C.estilo === 'guiado' ? 'Toque a tônica, a terça e a quinta de ' + ac.nome + ', em qualquer ordem.' : 'Quais são as três notas de ' + ac.nome + '?');
    pintar(); mandaAlvos();
  }
  function mandaAlvos() {
    const ms = [];
    for (let m = 40; m <= 79; m++) { const k = C.tri.indexOf(m % 12); if (k >= 0 && !C.achou[k]) ms.push(m); }
    Entrada.alvos(ms);
  }
  function ve(k) { return C.achou[k] || C.estilo === 'guiado' || C.dicas[k] || k === 0 || C.pronto; }

  function pintar() {
    const ac = C.ac, cores = coresPapel(ac), pap = papeis(ac);
    // roda da escala: as 7 notas de Dó maior; o acorde é "uma sim, uma não"
    const cx = 125, cy = 125, R = 82;
    const root = svg('svg', { viewBox: '0 0 250 250', class: 'roda', role: 'img', 'aria-label': 'Roda da escala de Dó maior com as notas de ' + ac.nome });
    root.appendChild(svg('circle', { cx: cx, cy: cy, r: R, class: 'rguia' }));
    const P = ESCALA_C.map((pc, i) => { const a = -Math.PI / 2 + i * 2 * Math.PI / 7; return { pc, a, x: cx + R * Math.cos(a), y: cy + R * Math.sin(a) }; });
    const ach = C.tri.map(pc => P[ESCALA_C.indexOf(pc)]).filter((_, k) => C.achou[k]);
    if (ach.length === 3) root.appendChild(svg('polygon', { points: ach.map(p => p.x.toFixed(1) + ',' + p.y.toFixed(1)).join(' '), class: 'rtri' }));
    else if (ach.length === 2) root.appendChild(svg('line', { x1: ach[0].x, y1: ach[0].y, x2: ach[1].x, y2: ach[1].y, class: 'rlinha' }));
    const t1 = svg('text', { x: cx, y: cy - 24, class: 'rgrau' }); t1.textContent = ac.grau; root.appendChild(t1);
    const t2 = svg('text', { x: cx, y: cy + 20, class: 'rnome' }); t2.textContent = ac.nome; root.appendChild(t2);
    P.forEach(p => {
      const k = C.tri.indexOf(p.pc), mostra = k >= 0 && ve(k);
      const g = svg('g', { class: 'rno' + (k >= 0 && C.achou[k] ? ' achou' : mostra ? (k === 0 ? ' raiz' : ' pista') : ''), transform: 'translate(' + p.x.toFixed(1) + ',' + p.y.toFixed(1) + ')', 'data-pc': p.pc });
      if (k >= 0) g.style.setProperty('--c', cores[k]);
      g.appendChild(svg('circle', { r: 20 }));
      const t = svg('text', { y: 5.5 }); t.textContent = abc(p.pc); g.appendChild(t);
      root.appendChild(g);
      if (mostra) {
        const tp = svg('text', { x: (cx + (R + 33) * Math.cos(p.a)).toFixed(1), y: (cy + (R + 33) * Math.sin(p.a) + 4.5).toFixed(1), class: 'rpapel' });
        tp.style.setProperty('--c', cores[k]); tp.textContent = pap[k]; root.appendChild(tp);
      }
    });
    const host = $('#cRoda'); host.textContent = ''; host.appendChild(root);

    // as três vagas
    const box = $('#cSlots'); box.textContent = '';
    C.tri.forEach((pc, k) => {
      const d = el('div', 'slot' + (C.achou[k] ? ' ok' : '')); d.style.setProperty('--c', cores[k]);
      d.appendChild(el('b', '', pap[k])); d.appendChild(el('span', '', ve(k) ? nomeNota(pc) : '?'));
      box.appendChild(d);
    });

    // braço: desenho do acorde (guiado) ou todas as notas naturais (pense rápido)
    if (C.estilo === 'guiado' || C.pronto) {
      const L = [];
      ac.shape.forEach((f, s) => { if (f < 0) return; const k = C.tri.indexOf((CORDAS[s] + f) % 12); L.push({ s, f, rot: pap[k], cor: cores[k], cls: C.achou[k] || C.pronto ? 'aceso' : '' }); });
      braco.pontos(L);
    } else {
      braco.pontos(NATURAIS.map(n => { const k = C.tri.indexOf(n.pc), ok = k >= 0 && C.achou[k]; return { s: n.s, f: n.f, rot: abc(n.pc), cor: ok ? cores[k] : '#cfc6dd', cls: ok ? 'aceso' : '' }; }));
    }
  }

  function estouraNo(pc, cor, n) {
    const g = $('#cRoda [data-pc="' + pc + '"]'); if (!g) return;
    const c = centroDe(g); FX.estouro(c.x, c.y, cor, n || 14);
  }

  function nota(ev) {
    if (!C || C.estado !== 'jogo') return;
    const pc = ev.midi % 12, k = C.tri.indexOf(pc), cores = coresPapel(C.ac), p = braco.onde(ev.midi);
    if (C.pronto) {                                              // acorde montado: tocar as cordas só faz festa
      if (k >= 0 && p) { const c = braco.centro(p.s, p.f); FX.estouro(c.x, c.y, cores[k], 8, 0.7); braco.acende(p.s, p.f); }
      return;
    }
    if (k >= 0) {
      clearTimeout(C.pend);
      if (C.achou[k]) return;
      C.achou[k] = true; C.pontos += 100;
      $('#cPontos').textContent = fmt(C.pontos); bump($('#cPontos'));
      pintar();
      estouraNo(pc, cores[k], 16);
      const q = braco.onde(ev.midi);
      if (q) { const c = braco.centro(q.s, q.f); FX.estouro(c.x, c.y, cores[k], 12); braco.acende(q.s, q.f); }
      fala($('#cFala'), nomeNota(pc) + ' é a ' + ['tônica', C.ac.menor ? 'terça menor' : 'terça maior', 'quinta justa'][k] + ' de ' + C.ac.nome + '.', 'bom');
      if (C.achou.every(Boolean)) completar(); else mandaAlvos();
      return;
    }
    if (ev.via === 'alvo' || !ev.ataque) return;
    clearTimeout(C.pend);
    if (ev.via === 'toque') errar(ev); else C.pend = setTimeout(() => errar(ev), 130);
  }

  function errar(ev) {
    if (!C || C.pronto) return;
    const pc = ev.midi % 12, p = braco.onde(ev.midi);
    C.erros++;
    if (p) braco.pisca(p.s, p.f, abc(pc));
    fala($('#cFala'), nomeNota(pc) + ' não entra em ' + C.ac.nome + '.' + (C.estilo === 'rapido' ? ' Na roda: uma nota sim, uma não.' : ''), 'ruim');
    if (C.estilo === 'rapido' && C.erros >= 2) darDica();
  }
  function darDica() {
    const k = C.achou.findIndex((a, i) => !a && !C.dicas[i] && i > 0);
    if (k < 0) return;
    C.dicas[k] = true; pintar();
  }

  function completar() {
    const ac = C.ac, bonus = Math.round(300 * clamp(1 - (agora() - C.em) / 20000, 0, 1));
    const limpo = C.erros === 0 && !C.dicas.some(Boolean);
    C.pronto = true; C.pontos += bonus + (limpo ? 100 : 0); if (limpo) C.limpos++;
    Entrada.alvos([]);
    pintar();
    $('#cPontos').textContent = fmt(C.pontos); bump($('#cPontos'));
    coresPapel(ac).forEach((cor, k) => estouraNo(C.tri[k], cor, 20));
    const midis = []; ac.shape.forEach((f, s) => { if (f >= 0) midis.push(CORDAS[s] + f); });
    if (Som.podeSoar()) Som.rasgueado(midis);
    fala($('#cFala'), (ac.menor ? 'Da T à 3m são 3 casas; da 3m à 5J, 4. Terça menor: acorde menor.' : 'Da T à 3M são 4 casas; da 3M à 5J, 3. Terça maior: acorde maior.') + ' Agora monte ' + ac.nome + ' e toque as cordas marcadas.', 'bom');
    const b = $('#cProx'); b.hidden = false; b.textContent = C.i + 1 < C.ordem.length ? 'Próximo acorde' : 'Ver resultado';
    C.auto = setTimeout(avancar, 7000);
  }
  function avancar() {
    if (!C || !C.pronto) return;
    clearTimeout(C.auto);
    if (C.i + 1 < C.ordem.length) { C.i++; carregar(); } else fim();
  }

  function update() {
    if (!C || C.estado !== 'jogo' || C.pronto) return;
    const dt = agora() - C.em;
    $('#cBonus').textContent = Math.round(300 * clamp(1 - dt / 20000, 0, 1));
    if (C.estilo === 'rapido' && dt > 12000 && !C.dicaTempo) { C.dicaTempo = true; darDica(); }
  }

  function fim() {
    const c = C, s = Store.get(), id = 'chord:' + c.estilo, ant = s.best[id], recorde = !ant || c.pontos > ant.p;
    clearTimeout(c.auto); clearTimeout(c.pend);
    c.estado = 'fim'; Entrada.alvos([]);
    const feitos = c.i + (c.pronto ? 1 : 0);
    s.best[id] = { p: Math.max(c.pontos, ant ? ant.p : 0) };
    contarRodada(); Store.save();
    const xp = Math.round(c.pontos / 10) + 20; ganharXP(xp);
    const guiado = c.estilo === 'guiado', todos = feitos === c.ordem.length;
    ir('fim', {
      olho: 'Monta-Acorde · ' + (guiado ? 'Guiado' : 'Pense rápido'),
      titulo: recorde && c.pontos > 0 ? 'Novo recorde' : 'Rodada concluída',
      pontos: c.pontos, estrelas: null, festa: todos,
      linhas: [['Acordes montados', feitos + ' de ' + c.ordem.length], ['Sem erro e sem dica', String(c.limpos)], ['XP ganho', '+' + xp], ['Campo harmônico', 'C · Dm · Em · F · G · Am']],
      msg: todos ? (guiado ? 'C, G, Am e F, nessa ordem, são a progressão I–V–vi–IV: a base de centenas de músicas.' : 'Você montou os seis acordes de cabeça. Leve a progressão para o Jam.') : 'Monte os seis para fechar o campo harmônico.',
      extra: todos ? { rotulo: 'Improvisar em C–G–Am–F', fn: () => ir('jam', { prog: [0, 4, 5, 3], tom: 9 }) } : null,
      deNovo: () => { ir('chord'); comecar(); }
    });
  }

  $$('#chEstilo button').forEach(b => b.addEventListener('click', () => { estilo = b.dataset.v; pintarPrep(); }));
  $('#chGo').addEventListener('click', comecar);
  $('#cProx').addEventListener('click', avancar);
  $('#chSair').addEventListener('click', () => { if (C && (C.i > 0 || C.pronto)) fim(); else ir('chord'); });
  return { entrar, sair, nota, update };
})();

/* ---------------- JAM ---------------- */
Telas.jam = (() => {
  const cfg = { fonte: 'base', tom: 9, prog: [5, 3, 0, 4], sel: 0, bpm: 84, arquivo: null, url: '', nome: '', mTon: 9, mMenor: true };
  let J = null, braco = null;
  const nomeTom = (t, menor) => pt(t, usaBemol(t, menor)) + (menor ? ' menor' : ' maior');
  const acordeDoGrau = (i, K) => { const g = GRAUS[i], raiz = (K + g.st) % 12; return { raiz, menor: g.menor, g: g.g, nome: abc(raiz, usaBemol(K, false)) + (g.menor ? 'm' : '') }; };

  function pintarPrep() {
    const base = cfg.fonte === 'base', K = (cfg.tom + 3) % 12;
    $$('#jamFonte button').forEach(b => b.classList.toggle('on', b.dataset.v === cfg.fonte));
    $('#jamBaseBox').hidden = !base; $('#jamMusicaBox').hidden = base;
    $$('#jamTom button').forEach(b => b.classList.toggle('on', +b.dataset.v === cfg.tom));
    const pr = $('#jamProg'); pr.textContent = '';
    cfg.prog.forEach((gi, i) => {
      const a = acordeDoGrau(gi, K), b = el('button', 'comp' + (i === cfg.sel ? ' sel' : '')); b.type = 'button';
      b.setAttribute('aria-label', 'Compasso ' + (i + 1) + ': ' + a.nome);
      b.appendChild(el('b', '', a.nome)); b.appendChild(el('small', '', a.g));
      b.addEventListener('click', () => { cfg.sel = i; pintarPrep(); });
      pr.appendChild(b);
    });
    const gr = $('#jamGraus'); gr.textContent = '';
    GRAUS.forEach((g, gi) => {
      const a = acordeDoGrau(gi, K), b = el('button', 'chip' + (cfg.prog[cfg.sel] === gi ? ' on' : '')); b.type = 'button';
      b.appendChild(document.createTextNode(a.nome)); b.appendChild(el('small', '', g.g));
      b.addEventListener('click', () => {
        cfg.prog[cfg.sel] = gi; cfg.sel = (cfg.sel + 1) % 4;
        try { Som.garante(); if (Som.podeSoar()) { const r = 48 + a.raiz; Som.rasgueado([r, r + (a.menor ? 3 : 4), r + 7, r + 12], 0.3); } } catch (e) { /* mudo */ }
        pintarPrep();
      });
      gr.appendChild(b);
    });
    $('#jamBpm').value = cfg.bpm; $('#jamBpmV').textContent = cfg.bpm;
    $('#jamFone').checked = !!Store.get().cfg.fone;
    $('#jamTomMusica').hidden = !cfg.arquivo;
    const j = janelaDoTom(cfg.mTon, cfg.mMenor);
    $('#jamTomNome').textContent = nomeTom(cfg.mTon, cfg.mMenor);
    $('#jamTomNome').title = 'Pentatônica nas casas ' + j.de + ' a ' + j.ate;
  }

  function entrar(arg) {
    J = null; Entrada.alvos([]);
    if (arg && arg.prog) { cfg.fonte = 'base'; cfg.prog = arg.prog.slice(); cfg.tom = arg.tom; cfg.sel = 0; }
    $('#jamPrep').hidden = false; $('#jamJogo').hidden = true;
    pintarPrep();
  }
  function parar() { Base.parar(); if (J && J.audio) { try { J.audio.pause(); } catch (e) {} J.audio.onended = null; } }
  function sair() { parar(); J = null; }

  async function comecar() {
    try { Som.garante(); } catch (e) { toast('Este navegador não toca áudio aqui.'); return; }
    const base = cfg.fonte === 'base';
    if (!base && !cfg.arquivo) { toast('Escolha um arquivo de áudio primeiro.'); return; }
    const tonica = base ? cfg.tom : cfg.mTon, menor = base ? true : cfg.mMenor, jan = janelaDoTom(tonica, menor), bemol = usaBemol(tonica, menor);
    J = { base, tonica, menor, bemol, conj: pentaDoTom(tonica, menor), pontos: 0, combo: 0, maxCombo: 0, dentro: 0, fora: 0, doAcorde: 0, rastro: [], uPc: -1, uT: 0, estado: 'jogo', acordes: null, audio: null, comp: -1 };
    $('#jamPrep').hidden = true; $('#jamJogo').hidden = false;
    braco = Braco($('#jamBraco'), { de: jan.de, ate: jan.ate, aoTocar: m => Entrada.toque(m) });
    const L = [];
    for (let s = 0; s < 6; s++) for (let f = jan.de; f <= jan.ate; f++) {
      const pc = (CORDAS[s] + f) % 12; if (J.conj.indexOf(pc) < 0) continue;
      const iv = (pc - tonica + 12) % 12; L.push({ s, f, rot: IV[iv], cor: IV_COR[iv], cls: iv === 0 ? 'tonica' : '' });
    }
    braco.pontos(L);
    Entrada.alvos([]);
    ['jDentro', 'jPontos', 'jCombo'].forEach(id => { $('#' + id).textContent = '0'; });
    const fx = $('#jamFaixa'); fx.textContent = '';
    if (base) {
      const K = (tonica + 3) % 12;
      J.acordes = cfg.prog.map(i => acordeDoGrau(i, K));
      const pr = el('div', 'prog');
      J.acordes.forEach(a => { const d = el('div', 'comp'); d.appendChild(el('b', '', a.nome)); d.appendChild(el('small', '', a.g)); pr.appendChild(d); });
      fx.appendChild(pr);
      J.soBateria = Som.origem() === 'mic' && !Store.get().cfg.fone;       // sem fone, baixo e acordes confundiriam o microfone
      Base.tocar(J.acordes, cfg.bpm, 6, () => { if (J && J.estado === 'jogo') fim(); }, J.soBateria);
    } else {
      const m = el('div', 'musica'); m.appendChild(el('b', '', cfg.nome));
      m.appendChild(el('span', 'dica', nomeTom(tonica, menor) + ' · pentatônica nas casas ' + jan.de + ' a ' + jan.ate));
      const pb = el('div', 'pbarra'), fill = el('i'); fill.id = 'jProg'; fill.style.transform = 'scaleX(0)'; pb.appendChild(fill); m.appendChild(pb);
      fx.appendChild(m);
      J.audio = new Audio(cfg.url); J.audio.volume = 0.85;
      J.audio.onended = () => { if (J && J.estado === 'jogo') fim(); };
      try { await J.audio.play(); } catch (e) { toast('Não consegui tocar esse arquivo aqui.'); }
    }
    fala($('#jFala'), J.soBateria ? 'Sem fone a base toca só a bateria, para o microfone ouvir o violão. A vermelha (T) é a casa.' : 'Qualquer bolinha serve. A vermelha (T) é a casa: comece e termine nela.');
    window.scrollTo(0, 0);
  }

  function nota(ev) {
    if (!J || J.estado !== 'jogo') return;
    const pc = ev.midi % 12, t = agora();
    if (!ev.ataque && pc === J.uPc && t - J.uT < 1500) return;      // mesma nota "mudando" de oitava ao morrer: ignora
    J.uPc = pc; J.uT = t;
    const dentro = J.conj.indexOf(pc) >= 0, iv = (pc - J.tonica + 12) % 12, cor = dentro ? IV_COR[iv] : '#8a8399';
    let doAc = false;
    if (dentro && J.base) { const pos = Base.posicao(); if (pos) doAc = triade(J.acordes[pos.compasso]).indexOf(pc) >= 0; }
    const p = braco.onde(ev.midi), c = p ? braco.centro(p.s, p.f) : centroDe($('#jamRastro'));
    if (dentro) {
      J.combo++; J.dentro++; if (J.combo > J.maxCombo) J.maxCombo = J.combo;
      const pts = Math.round((doAc ? 25 : 10) * (1 + Math.min(4, Math.floor(J.combo / 8)) * 0.5));
      J.pontos += pts; if (doAc) J.doAcorde++;
      FX.estouro(c.x, c.y, cor, doAc ? 20 : 9, doAc ? 1.1 : 0.7);
      if (p && braco.ponto(p.s, p.f)) acendeUmPouco(braco, p.s, p.f, 380);
      if (doAc) flutua(c.x, c.y - 20, 'acorde +' + pts, cor);
      if (J.combo === 8 || J.combo === 16 || J.combo === 24) fala($('#jFala'), J.combo + ' notas seguidas na escala.', 'bom');
    } else {
      J.combo = 0; J.fora++;
      if (p) braco.pisca(p.s, p.f, abc(pc, J.bemol));
      fala($('#jFala'), nomeNota(pc, J.bemol) + ' está fora da pentatônica. Volte para as bolinhas.', 'ruim');
    }
    J.rastro.push({ t, midi: ev.midi, cor, g: doAc });
    $('#jDentro').textContent = J.dentro; $('#jPontos').textContent = fmt(J.pontos); $('#jCombo').textContent = J.combo;
  }

  function update() {
    if (!J || J.estado !== 'jogo') return;
    if (J.base) {
      const pos = Base.posicao();
      if (pos && pos.compasso !== J.comp) { J.comp = pos.compasso; $$('#jamFaixa .comp').forEach((e, i) => e.classList.toggle('on', i === pos.compasso)); }
    } else if (J.audio && J.audio.duration) {
      const e = $('#jProg'); if (e) e.style.transform = 'scaleX(' + clamp(J.audio.currentTime / J.audio.duration, 0, 1).toFixed(3) + ')';
    }
    // rastro: cada nota vira um ponto colorido que corre para a esquerda
    const cv = $('#jamRastro'), w = cv.clientWidth, h = cv.clientHeight, dpr = Math.min(2, window.devicePixelRatio || 1);
    if (!w) return;
    if (cv.width !== Math.round(w * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr); }
    const g = cv.getContext('2d'), t = agora(), y = m => h - 9 - (clamp(m, 40, 79) - 40) / 39 * (h - 18);
    g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h);
    g.strokeStyle = 'rgba(255,89,100,.28)'; g.lineWidth = 1;
    for (let m = 40; m <= 79; m++) if (m % 12 === J.tonica) { g.beginPath(); g.moveTo(0, y(m)); g.lineTo(w, y(m)); g.stroke(); }
    let corta = 0;
    for (let i = 0; i < J.rastro.length; i++) {
      const r = J.rastro[i], idade = (t - r.t) / 1000, x = w - 18 - idade * 70;
      if (x < -14) { corta = i + 1; continue; }
      g.globalAlpha = clamp(1.15 - idade * 0.1, 0.3, 1); g.fillStyle = r.cor;
      g.beginPath(); g.arc(x, y(r.midi), (r.g ? 8 : 6) + Math.max(0, 6 - idade * 24), 0, 6.283); g.fill();
    }
    if (corta) J.rastro.splice(0, corta);
    g.globalAlpha = 1;
  }

  function fim() {
    const j = J, s = Store.get(), ant = s.best.jam, recorde = !ant || j.pontos > ant.p;
    j.estado = 'fim'; parar();
    const tot = j.dentro + j.fora;
    s.best.jam = { p: Math.max(j.pontos, ant ? ant.p : 0) };
    if (tot >= 8) contarRodada();
    Store.save();
    const xp = tot >= 8 ? Math.round(j.pontos / 10) + 20 : 0; if (xp) ganharXP(xp);
    const linhas = [['Notas tocadas', String(tot)], ['Dentro da escala', tot ? Math.round(j.dentro / tot * 100) + '%' : '–'], ['Maior sequência', String(j.maxCombo)]];
    if (j.base) linhas.push(['Notas do acorde da vez', String(j.doAcorde)]);
    linhas.push(['Tom', nomeTom(j.tonica, j.menor)], ['XP ganho', '+' + xp]);
    ir('fim', {
      olho: 'Jam · ' + (j.base ? 'base do jogo' : cfg.nome),
      titulo: tot < 8 ? 'Jam encerrado' : recorde ? 'Novo recorde' : 'Jam concluído',
      pontos: j.pontos, estrelas: null, festa: tot >= 30,
      linhas,
      msg: tot < 8 ? 'Toque pelo menos 8 notas para o jam contar como rodada.' :
        j.base ? 'Próximo passo: mirar na nota do acorde bem na troca de compasso. É o que faz um solo soar "certo".' : 'Se alguma nota soou estranha o jogo todo, o tom estimado pode estar errado: ajuste meio tom e tente de novo.',
      deNovo: () => { ir('jam'); comecar(); }
    });
  }

  $$('#jamFonte button').forEach(b => b.addEventListener('click', () => { cfg.fonte = b.dataset.v; pintarPrep(); }));
  $$('#jamTom button').forEach(b => b.addEventListener('click', () => { cfg.tom = +b.dataset.v; pintarPrep(); }));
  $('#jamBpm').addEventListener('input', e => { cfg.bpm = +e.target.value; $('#jamBpmV').textContent = cfg.bpm; });
  $('#jamFone').addEventListener('change', e => { Store.get().cfg.fone = e.target.checked; Store.save(); });
  $('#jamTomMenos').addEventListener('click', () => { cfg.mTon = (cfg.mTon + 11) % 12; pintarPrep(); });
  $('#jamTomMais').addEventListener('click', () => { cfg.mTon = (cfg.mTon + 1) % 12; pintarPrep(); });
  $('#jamTomModo').addEventListener('click', () => { cfg.mMenor = !cfg.mMenor; pintarPrep(); });
  $('#jamGo').addEventListener('click', comecar);
  $('#jamSair').addEventListener('click', () => { if (J && J.estado === 'jogo') fim(); });
  $('#jamArq').addEventListener('change', async e => {
    const f = e.target.files && e.target.files[0]; if (!f) return;
    if (cfg.url) URL.revokeObjectURL(cfg.url);
    cfg.arquivo = f; cfg.url = URL.createObjectURL(f); cfg.nome = f.name;
    const info = $('#jamArqInfo'); info.textContent = f.name + ': estimando o tom…';
    pintarPrep();
    try {
      const ctx = Som.garante(), bytes = await f.arrayBuffer();
      const ab = await new Promise((ok, no) => ctx.decodeAudioData(bytes, ok, no));
      const n = Math.min(ab.length, Math.floor(ab.sampleRate * 150)), mono = new Float32Array(n);
      for (let c = 0; c < ab.numberOfChannels; c++) { const d = ab.getChannelData(c); for (let i = 0; i < n; i++) mono[i] += d[i]; }
      await new Promise(r => setTimeout(r, 30));
      const k = detectKey(mono, ab.sampleRate);
      if (k) { cfg.mTon = k.tonica; cfg.mMenor = k.menor; info.textContent = f.name + ': tom estimado ' + nomeTom(k.tonica, k.menor) + '.'; }
      else info.textContent = f.name + ': curta demais para estimar o tom. Escolha abaixo.';
    } catch (err) { info.textContent = f.name + ': não consegui analisar o áudio. Escolha o tom abaixo.'; }
    pintarPrep();
  });
  return { entrar, sair, nota, update };
})();

/* ---------------- AFINADOR E TESTE DE SOM ---------------- */
Telas.afina = (() => {
  const ok = [false, false, false, false, false, false], acum = [0, 0, 0, 0, 0, 0];
  let mfS = 0, uNota = null, uT = 0, escolhido = '', nLog = 0;

  function pintarCordas() {
    const ol = $('#aCordas'); ol.textContent = '';
    CORDAS.forEach((m, i) => { const li = el('li', ok[i] ? 'ok' : ''); li.appendChild(el('b', '', pt(m % 12))); li.appendChild(el('small', '', CORDA_NOME[i] + ' corda')); ol.appendChild(li); });
  }
  function pintarDiag() {
    const i = Som.info(), dl = $('#aDiag'); dl.textContent = '';
    const liga = v => (v === true ? 'ligado' : v === false ? 'desligado' : 'não informado');
    const L = [['Entrada', i.origem === 'mic' ? 'microfone' : i.origem === 'arquivo' ? 'gravação' : 'desligada']];
    if (i.taxa) {
      L.push(['Análise', i.caminho || '–'], ['Taxa', i.taxa + ' Hz'], ['Janela', i.janelaMs.toFixed(1).replace('.', ',') + ' ms'], ['Passo', i.passoMs.toFixed(1).replace('.', ',') + ' ms']);
      if (i.saidaLat != null) L.push(['Latência de saída', i.saidaLat.toFixed(0) + ' ms']);
      L.push(['Piso de ruído', (20 * Math.log10(Entrada.tr.piso())).toFixed(0) + ' dB']);
    }
    if (i.origem === 'mic' && i.track) L.push(['Cancelamento de eco', liga(i.track.echoCancellation)], ['Redução de ruído', liga(i.track.noiseSuppression)], ['Ganho automático', liga(i.track.autoGainControl)]);
    L.forEach(([k, v]) => { const w = el('div'); w.appendChild(el('dt', '', k)); w.appendChild(el('dd', '', v)); dl.appendChild(w); });
    if (uNota === null) $('#aCorda').textContent = Som.ligado() ? 'Toque uma corda solta.' : 'Ligue o microfone para começar.';
  }
  async function listar() {
    const sel = $('#aEntrada'), ds = await Som.entradas();
    sel.textContent = ''; sel.appendChild(new Option('Padrão do aparelho', ''));
    ds.forEach((d, i) => { if (d.deviceId && d.deviceId !== 'default') sel.appendChild(new Option(d.label || 'Microfone ' + (i + 1), d.deviceId)); });
    sel.value = escolhido; if (sel.value !== escolhido) sel.value = '';
  }

  function entrar() { pintarCordas(); pintarDiag(); listar(); Entrada.alvos([]); }

  function update(ts, dt) {
    const fr = Som.ultimo, pont = $('#aPonteiro');
    if (!Som.ligado() || !fr) { pont.classList.remove('vivo'); return; }
    if (fr.rms > Entrada.tr.gate() && fr.c >= 0.9 && fr.f > 60) {
      const mf = 69 + 12 * Math.log(fr.f / 440) / Math.LN2, n = Math.round(mf);
      mfS = n === uNota ? mfS * 0.75 + mf * 0.25 : mf; uNota = n; uT = agora();
      let bi = 0, bd = 99;
      CORDAS.forEach((m, i) => { const d = Math.abs(mfS - m); if (d < bd) { bd = d; bi = i; } });
      const cents = (mfS - n) * 100, dif = mfS - CORDAS[bi], perto = bd <= 2.5;
      $('#aNota').textContent = pt(n % 12);
      $('#aCorda').textContent = !perto ? abc(n % 12) + ' · ' + (cents >= 0 ? '+' : '−') + Math.abs(cents).toFixed(0) + ' cents'
        : Math.abs(dif) <= 0.08 ? CORDA_NOME[bi] + ' corda (' + pt(CORDAS[bi] % 12) + ') afinada'
          : CORDA_NOME[bi] + ' corda (' + pt(CORDAS[bi] % 12) + '): ' + (dif < 0 ? 'grave, aperte a tarraxa' : 'aguda, afrouxe a tarraxa');
      pont.classList.add('vivo'); pont.style.left = clamp(50 + (perto ? dif * 100 : cents), 2, 98) + '%';
      if (perto && Math.abs(dif) <= 0.08) {
        acum[bi] += dt;
        if (acum[bi] > 0.3 && !ok[bi]) {
          ok[bi] = true; pintarCordas();
          const c = centroDe($$('#aCordas li')[bi]); FX.estouro(c.x, c.y, '#4ade80', 16);
          if (ok.every(Boolean)) {
            const s = Store.get();
            if (s.afinou !== hojeStr()) { s.afinou = hojeStr(); Store.save(); ganharXP(30); toast('Seis cordas afinadas: +30 XP'); }
            FX.confete();
          }
        }
      } else acum[bi] = 0;
    } else if (agora() - uT > 1200) pont.classList.remove('vivo');
  }

  function nota(ev) {                                           // registro das notas reconhecidas
    const ul = $('#aLog'), li = el('li', '', nomeNota(ev.midi % 12));
    li.style.setProperty('--c', IV_COR[((ev.midi - 40) % 12 + 12) % 12]);
    ul.appendChild(li); nLog++;
    while (ul.children.length > 40) ul.removeChild(ul.firstChild);
  }

  $('#aEntrada').addEventListener('change', async e => {
    escolhido = e.target.value;
    if (Som.origem() === 'mic') { try { await Som.ligarMic(escolhido); pintarDiag(); } catch (err) { toast('Não consegui abrir essa entrada.'); } }
  });
  $('#aLat').addEventListener('click', async () => {
    const out = $('#aLatOut'), i = Som.info();
    if (!Som.ligado() || i.origem !== 'mic') { out.textContent = 'Ligue o microfone primeiro (botão no topo).'; return; }
    if (i.caminho !== 'AudioWorklet') { out.textContent = 'Este navegador caiu no modo de análise simples, sem medição de latência.'; return; }
    out.textContent = 'Medindo: três cliques no alto-falante. Sem fone e com volume alto.';
    const vs = [];
    for (let k = 0; k < 3; k++) {
      const v = await Som.medirLatencia();
      if (v != null && v > 0.003 && v < 0.9) vs.push(v * 1000);
      await new Promise(r => setTimeout(r, 350));
    }
    if (!vs.length) { out.textContent = 'Não ouvi o clique. Aumente o volume, tire o fone e tente de novo.'; return; }
    vs.sort((a, b) => a - b);
    out.textContent = 'Ida e volta (alto-falante, ar, microfone): ' + vs[vs.length >> 1].toFixed(0) + ' ms. O microfone responde por uma parte disso; a análise soma cerca de 30 a 50 ms até a tela reagir.';
  });
  $('#aArq').addEventListener('change', async e => {
    const f = e.target.files && e.target.files[0]; if (!f) return;
    const out = $('#aLatOut'); $('#aLog').textContent = ''; nLog = 0;
    out.textContent = f.name + ': tocando e analisando…';
    try {
      const bytes = await f.arrayBuffer();
      await Som.arquivoComoEntrada(bytes, () => { out.textContent = f.name + ': ' + nLog + ' notas reconhecidas.'; pintarDiag(); });
      pintarDiag();
    } catch (err) { out.textContent = 'Não consegui abrir esse arquivo de áudio.'; }
    e.target.value = '';
  });
  return { entrar, update, nota, listar, pintarDiag, escolhido: () => escolhido };
})();

Telas.plano = {};

/* ---------------- FIM DE RODADA ---------------- */
Telas.fim = (() => {
  let d = null;
  function entrar(x) {
    d = x;
    $('#fOlho').textContent = d.olho; $('#fTitulo').textContent = d.titulo;
    const es = $('#fEstrelas'); es.textContent = ''; es.hidden = d.estrelas == null;
    if (d.estrelas != null) for (let i = 0; i < 3; i++) es.appendChild(el('span', i < d.estrelas ? 'on' : '', '★'));
    $('#fPontos').textContent = fmt(d.pontos);
    const dl = $('#fStats'); dl.textContent = '';
    d.linhas.forEach(([k, v]) => { const w = el('div'); w.appendChild(el('dt', '', k)); w.appendChild(el('dd', '', v)); dl.appendChild(w); });
    fala($('#fMsg'), d.msg || '');
    const ex = $('#fExtra'); ex.hidden = !d.extra; if (d.extra) ex.textContent = d.extra.rotulo;
    if (d.festa) FX.confete();
  }
  $('#fDeNovo').addEventListener('click', () => { if (d) d.deNovo(); });
  $('#fExtra').addEventListener('click', () => { if (d && d.extra) d.extra.fn(); });
  $('#fInicio').addEventListener('click', () => ir('home'));
  return { entrar };
})();
