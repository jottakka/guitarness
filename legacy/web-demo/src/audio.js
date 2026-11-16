/* =====================================================================
   Áudio: microfone -> filtros -> AudioWorklet (detector) -> rastreador
          + sintetizador (corda dedilhada, base de bateria/baixo/acordes)
   ===================================================================== */
const Som = (() => {
  let ctx = null, fonte = null, stream = null, hp = null, lp2 = null, node = null, analyser = null, saida = null, mestre = null;
  let estado = 'off';                 // off | on
  let origem = '';                    // mic | arquivo
  let caminho = '';                   // AudioWorklet | AnalyserNode
  let D = 2, W = 1024, hop = 256, fsd = 24000;
  let det = null, buf = null, lin = null, ultimoPoll = 0, alvosF = [], alvosId = 0;
  let aoQuadro = null, probeCb = null, trackCfg = null;
  const cachePluck = new Map();
  const api = { ultimo: null };

  function garante() {
    if (!ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) throw new Error('Este navegador não tem Web Audio.');
      try { ctx = new AC({ latencyHint: 'interactive' }); } catch (e) { ctx = new AC(); }
      mestre = ctx.createGain(); mestre.gain.value = 0.9; mestre.connect(ctx.destination);
    }
    if (ctx.state === 'suspended') { const p = ctx.resume(); if (p && p.catch) p.catch(() => {}); }
    return ctx;
  }

  async function montaCadeia() {
    if (hp) return;
    D = Math.max(1, Math.round(ctx.sampleRate / 24000)); fsd = ctx.sampleRate / D;
    W = 2 * Math.round(fsd * 0.0427 / 2); hop = Math.round(fsd * 0.0107);
    hp = ctx.createBiquadFilter(); hp.type = 'highpass'; hp.frequency.value = 62; hp.Q.value = 0.707;
    const lp1 = ctx.createBiquadFilter(); lp1.type = 'lowpass'; lp1.frequency.value = 2600; lp1.Q.value = 0.707;
    lp2 = ctx.createBiquadFilter(); lp2.type = 'lowpass'; lp2.frequency.value = 2600; lp2.Q.value = 0.707;
    hp.connect(lp1); lp1.connect(lp2);
    saida = ctx.createGain(); saida.gain.value = 0; saida.connect(ctx.destination);   // mantém o grafo ativo, sem som
    try {
      if (!ctx.audioWorklet || !window.AudioWorkletNode) throw new Error('sem worklet');
      const fonteWl = workletSource();
      try { await ctx.audioWorklet.addModule(URL.createObjectURL(new Blob([fonteWl], { type: 'application/javascript' }))); }
      catch (e1) {                      // arquivo aberto direto do disco: blob é barrado (origem nula), data: passa
        await ctx.audioWorklet.addModule('data:application/javascript;charset=utf-8,' + encodeURIComponent(fonteWl));
      }
      node = new AudioWorkletNode(ctx, 'tonica-proc', {
        numberOfInputs: 1, numberOfOutputs: 1, channelCount: 1, channelCountMode: 'explicit',
        processorOptions: { D: D, W: W, hop: hop }
      });
      node.port.onmessage = e => {
        const m = e.data;
        if (m.probe !== undefined) { if (probeCb) { const cb = probeCb; probeCb = null; cb(m.probe); } return; }
        quadro(m);
      };
      lp2.connect(node); node.connect(saida);
      caminho = 'AudioWorklet';
    } catch (e) {                       // plano B: AnalyserNode lido a cada quadro de tela
      node = null;
      analyser = ctx.createAnalyser();
      let n = 2048; while (n < W * D) n *= 2;
      analyser.fftSize = n; analyser.smoothingTimeConstant = 0;
      lp2.connect(analyser); analyser.connect(saida);
      buf = new Float32Array(n); lin = new Float32Array(W);
      det = makeDetector(fsd, W);
      caminho = 'AnalyserNode';
    }
    if (alvosF.length) alvos(alvosF, alvosId);
  }

  function quadro(m) { api.ultimo = m; if (aoQuadro) aoQuadro(m); }

  function poll() {                    // só no plano B
    if (!analyser || estado !== 'on') return;
    const t = ctx.currentTime;
    if (t - ultimoPoll < 0.008) return;
    analyser.getFloatTimeDomainData(buf);
    const off = buf.length - W * D;
    for (let i = 0; i < W; i++) { let a = 0; const b = off + i * D; for (let k = 0; k < D; k++) a += buf[b + k]; lin[i] = a / D; }
    const passo = clamp(Math.round((t - ultimoPoll) * fsd), 16, W);
    ultimoPoll = t;
    const r = det.analyze(lin, passo), p = alvosF.map(f => det.presence(f));
    quadro({ t: t, f: r.f, c: r.c, rms: r.rms, rmsS: r.rmsS, p: p, tid: alvosId });
  }

  function soltaFonte() {
    if (fonte) { try { fonte.disconnect(); } catch (e) {} try { if (fonte.stop) fonte.stop(); } catch (e) {} fonte = null; }
    if (stream) { stream.getTracks().forEach(t => t.stop()); stream = null; }
    estado = 'off'; origem = ''; api.ultimo = null;
  }

  async function ligarMic(deviceId) {
    garante();
    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      const e = new Error('sem getUserMedia'); e.name = 'NotSupportedError'; throw e;
    }
    // processamento de voz desligado: ele deforma notas sustentadas
    const cru = { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 1 };
    if (deviceId) cru.deviceId = { exact: deviceId };
    let st;
    try { st = await navigator.mediaDevices.getUserMedia({ audio: cru }); }
    catch (e) {
      if (e && (e.name === 'OverconstrainedError' || e.name === 'TypeError')) st = await navigator.mediaDevices.getUserMedia({ audio: true });
      else throw e;
    }
    await montaCadeia();
    soltaFonte();
    stream = st;
    fonte = ctx.createMediaStreamSource(st); fonte.connect(hp);
    const tr = st.getAudioTracks()[0];
    trackCfg = tr && tr.getSettings ? tr.getSettings() : null;
    if (tr) tr.onended = () => { if (origem === 'mic') { soltaFonte(); if (api.aoMudar) api.aoMudar(); } };
    estado = 'on'; origem = 'mic';
    if (api.aoMudar) api.aoMudar();
  }

  async function arquivoComoEntrada(arrayBuffer, aoFim) {
    garante(); await montaCadeia();
    const ab = await new Promise((ok, no) => ctx.decodeAudioData(arrayBuffer, ok, no));
    soltaFonte();
    const s = ctx.createBufferSource(); s.buffer = ab;
    s.connect(hp); s.connect(mestre);
    s.onended = () => { if (fonte === s) { soltaFonte(); if (api.aoMudar) api.aoMudar(); if (aoFim) aoFim(); } };
    s.start();
    fonte = s; estado = 'on'; origem = 'arquivo';
    if (api.aoMudar) api.aoMudar();
    return ab.duration;
  }

  function desligar() { soltaFonte(); if (api.aoMudar) api.aoMudar(); }

  /* violão virtual: injeta cordas sintéticas direto na análise (testes sem instrumento) */
  async function ligarVirtual() {
    garante(); await montaCadeia(); soltaFonte();
    estado = 'on'; origem = 'virtual';
    if (api.aoMudar) api.aoMudar();
  }
  function virtual(midi, amp) {
    if (!hp) return;
    const d = ksPluck(ctx.sampleRate, mtof(midi), 1.4);
    const b = ctx.createBuffer(1, d.length, ctx.sampleRate); b.getChannelData(0).set(d);
    const s = ctx.createBufferSource(), g = ctx.createGain();
    s.buffer = b; g.gain.value = amp || 0.2; s.connect(g); g.connect(hp); s.start();
  }

  function alvos(freqs, id) {
    alvosF = freqs; alvosId = id;
    if (node) node.port.postMessage({ targets: freqs, tid: id });
  }

  /* ----- sons do jogo ----- */
  function podeSoar() { return origem !== 'mic' || Store.get().cfg.fone; }
  function dedilha(midi, vol, quando) {
    if (!ctx) return;
    let b = cachePluck.get(midi);
    if (!b) {
      const d = ksPluck(ctx.sampleRate, mtof(midi), 1.7);
      b = ctx.createBuffer(1, d.length, ctx.sampleRate); b.getChannelData(0).set(d);
      cachePluck.set(midi, b);
    }
    const s = ctx.createBufferSource(), g = ctx.createGain();
    s.buffer = b; g.gain.value = vol == null ? 0.5 : vol; s.connect(g); g.connect(mestre);
    s.start(quando || 0);
  }
  function rasgueado(midis, vol) {
    if (!ctx) return;
    const t = ctx.currentTime + 0.02;
    midis.forEach((m, i) => dedilha(m, vol == null ? 0.38 : vol, t + i * 0.045));
  }

  /* ----- latência de ida e volta: clique no alto-falante, espera no microfone ----- */
  function medirLatencia() {
    return new Promise(resolve => {
      if (!node || estado !== 'on' || origem !== 'mic') { resolve(null); return; }
      const t0 = ctx.currentTime + 0.3;
      const o = ctx.createOscillator(), g = ctx.createGain();
      o.type = 'square'; o.frequency.value = 1500;
      g.gain.setValueAtTime(0, 0); g.gain.setValueAtTime(0.9, t0); g.gain.setValueAtTime(0, t0 + 0.004);
      o.connect(g); g.connect(ctx.destination); o.start(t0 - 0.05); o.stop(t0 + 0.05);
      const base = api.ultimo ? api.ultimo.rms : 0.002;
      const lim = setTimeout(() => { probeCb = null; resolve(-1); }, 1900);
      probeCb = v => { clearTimeout(lim); resolve(v); };
      node.port.postMessage({ probe: t0, thr: Math.max(0.02, base * 10) });
    });
  }

  async function entradas() {
    try { const ds = await navigator.mediaDevices.enumerateDevices(); return ds.filter(d => d.kind === 'audioinput'); }
    catch (e) { return []; }
  }

  function info() {
    return {
      estado, origem, caminho, taxa: ctx ? ctx.sampleRate : 0, D, W, hop, fsd,
      janelaMs: W / fsd * 1000, passoMs: hop / fsd * 1000,
      baseLat: ctx && ctx.baseLatency ? ctx.baseLatency * 1000 : null,
      saidaLat: ctx && ctx.outputLatency ? ctx.outputLatency * 1000 : null,
      track: trackCfg
    };
  }

  Object.assign(api, {
    garante, ligarMic, arquivoComoEntrada, desligar, ligarVirtual, virtual, alvos, poll, podeSoar, dedilha, rasgueado, medirLatencia, entradas, info,
    aoMudar: null,
    aoQuadro(fn) { aoQuadro = fn; },
    ctx() { return ctx; }, mestre() { return mestre; },
    tempo() { return ctx ? ctx.currentTime : 0; },
    ligado() { return estado === 'on'; },
    origem() { return origem; }
  });
  return api;
})();

/* ---------- entrada unificada: microfone ou toque na tela ---------- */
const Entrada = (() => {
  const ouvintes = new Set();
  let tid = 0, ataque = 0;
  const tr = makeTracker(ev => {
    if (ev.type === 'ataque') ataque = agora();
    else emitir(ev);
  });
  function emitir(ev) { ouvintes.forEach(fn => { try { fn(ev); } catch (e) { console.error(e); } }); }
  Som.aoQuadro(fr => tr.push(fr));
  return {
    tr,
    escutar(fn) { ouvintes.add(fn); },
    alvos(midis) { tid++; tr.setTargets(midis, tid, Som.tempo()); Som.alvos(midis.map(mtof), tid); },
    toque(midi) {
      try { Som.garante(); if (Som.podeSoar()) Som.dedilha(midi, 0.5); } catch (e) { /* sem áudio: segue mudo */ }
      emitir({ type: 'nota', midi: midi, cents: 0, ataque: true, via: 'toque', t: 0 });
    },
    ultimoAtaque() { return ataque; }
  };
})();

/* ---------- base do Jam: bateria, baixo e acordes agendados no relógio de áudio ---------- */
const Base = (() => {
  let timer = 0, t0 = 0, batida = 0, proxima = 0, dur = 0, total = 0, prog = [], tocando = false, bus = null, ruido = null, aoFim = null, soBat = false;

  function ruidoBuf(ctx) {
    if (ruido) return ruido;
    ruido = ctx.createBuffer(1, ctx.sampleRate * 0.4, ctx.sampleRate);
    const d = ruido.getChannelData(0); for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
    return ruido;
  }
  function env(ctx, t, pico, fim) {
    const g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t); g.gain.exponentialRampToValueAtTime(pico, t + 0.006); g.gain.exponentialRampToValueAtTime(0.0001, t + fim);
    g.connect(bus); return g;
  }
  function bumbo(ctx, t) {
    const o = ctx.createOscillator(); o.frequency.setValueAtTime(140, t); o.frequency.exponentialRampToValueAtTime(46, t + 0.12);
    o.connect(env(ctx, t, 0.9, 0.2)); o.start(t); o.stop(t + 0.22);
  }
  function chiado(ctx, t, tipo, freq, pico, fim) {
    const s = ctx.createBufferSource(); s.buffer = ruidoBuf(ctx);
    const f = ctx.createBiquadFilter(); f.type = tipo; f.frequency.value = freq; f.Q.value = 0.8;
    s.connect(f); f.connect(env(ctx, t, pico, fim)); s.start(t); s.stop(t + fim + 0.02);
  }
  function baixo(ctx, t, midi, d) {
    const o = ctx.createOscillator(); o.type = 'triangle'; o.frequency.value = mtof(midi);
    o.connect(env(ctx, t, 0.5, d)); o.start(t); o.stop(t + d + 0.02);
  }
  function pad(ctx, t, midis, d) {
    const f = ctx.createBiquadFilter(); f.type = 'lowpass'; f.frequency.value = 1200;
    const g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t); g.gain.exponentialRampToValueAtTime(0.085, t + 0.08);
    g.gain.setValueAtTime(0.085, t + d - 0.2); g.gain.exponentialRampToValueAtTime(0.0001, t + d);
    f.connect(g); g.connect(bus);
    midis.forEach(m => [-6, 6].forEach(c => {
      const o = ctx.createOscillator(); o.type = 'sawtooth'; o.frequency.value = mtof(m); o.detune.value = c;
      o.connect(f); o.start(t); o.stop(t + d + 0.02);
    }));
  }
  function agenda(i, t) {
    const ctx = Som.ctx(), b = i % 4, ac = prog[Math.floor(i / 4) % prog.length];
    if (b === 0 || b === 2) bumbo(ctx, t); else chiado(ctx, t, 'bandpass', 1700, 0.3, 0.13);
    chiado(ctx, t, 'highpass', 7000, 0.09, 0.04); chiado(ctx, t + dur / 2, 'highpass', 7000, 0.05, 0.04);
    if (soBat) return;
    if (b === 0 || b === 2) baixo(ctx, t, ac.baixo, dur * 1.7);
    if (b === 0) pad(ctx, t, ac.notas, dur * 4);
  }
  function tique() {
    if (!tocando) return;
    const ctx = Som.ctx();
    while (proxima < ctx.currentTime + 0.25 && batida < total) { agenda(batida, proxima); batida++; proxima += dur; }
    if (batida >= total && ctx.currentTime > proxima) { parar(); if (aoFim) aoFim(); }
  }
  /* acordes: [{raiz, menor}] um por compasso; voltas: quantas vezes repete */
  function tocar(acordes, bpm, voltas, fim, soBateria) {
    soBat = !!soBateria;
    const ctx = Som.garante();
    parar();
    bus = ctx.createGain(); bus.gain.value = 0.55; bus.connect(Som.mestre());
    prog = acordes.map(a => {
      const r = 48 + ((a.raiz % 12) + 12) % 12;               // Dó3..Si3
      return { baixo: r - 12 >= 40 ? r - 12 : r, notas: [r, r + (a.menor ? 3 : 4), r + 7] };   // baixo nunca abaixo do Mi grave
    });
    dur = 60 / bpm; total = acordes.length * 4 * voltas; batida = 0;
    t0 = ctx.currentTime + 0.15; proxima = t0; tocando = true; aoFim = fim;
    timer = setInterval(tique, 50); tique();
  }
  function parar() {
    tocando = false; clearInterval(timer);
    if (bus) { const b = bus, ctx = Som.ctx(); try { b.gain.setTargetAtTime(0, ctx.currentTime, 0.05); } catch (e) {} setTimeout(() => { try { b.disconnect(); } catch (e) {} }, 400); bus = null; }
  }
  function posicao() {                                         // {compasso, volta, frac} ou null
    if (!tocando) return null;
    const el = Som.tempo() - t0; if (el < 0) return { compasso: 0, volta: 0, frac: 0, batida: 0 };
    const bt = el / dur, comp = Math.floor(bt / 4);
    return { compasso: comp % prog.length, volta: Math.floor(comp / prog.length), frac: bt / total, batida: Math.floor(bt) % 4 };
  }
  return { tocar, parar, posicao, tocando: () => tocando };
})();
