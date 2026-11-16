/* =====================================================================
   Rastreador de notas — transforma quadros do detector em eventos de jogo.
   Dois caminhos:
   1) "alvo": a nota que o jogo espera apareceu logo depois de um ataque?
      (rápido e tolerante a outras cordas ainda soando)
   2) "mpm": qual nota única e limpa está soando? (usado para nota livre,
      afinador e para dizer o que foi tocado de errado)
   ===================================================================== */
function makeTracker(emit) {
  var C_ON = 0.86;      // clareza mínima do MPM para aceitar uma nota
  var P_ON = 0.55;      // presença mínima do alvo (2 quadros seguidos)
  var P_FAST = 0.85;    // presença que dispensa o 2º quadro
  var S;

  function reset() {
    S = {
      nf: 0.003, cur: null, cand: null, run: 0, silent: 0,
      e0: 0, e1: 0, e2: 0, onsetT: -9, fresh: false, hadNote: false, rmsPre: 0, lastRms: 0,
      targets: [], tid: 0, shownT: 0, tRun: [], tPre: [], h0: [], h1: [], h2: [],
      cents: 0
    };
  }
  reset();

  function gate() { var g = S.nf * 4; return g < 0.002 ? 0.002 : g; }

  function setTargets(midis, tid, shownT) {
    S.targets = midis.slice(); S.tid = tid; S.shownT = shownT;
    S.tRun = []; S.tPre = []; S.h0 = []; S.h1 = []; S.h2 = [];
    for (var k = 0; k < midis.length; k++) { S.tRun.push(0); S.tPre.push(1); S.h0.push(1); S.h1.push(1); S.h2.push(1); }
  }

  function push(fr) {
    var t = fr.t, f = fr.f, c = fr.c, rms = fr.rms, rmsS = fr.rmsS, k;

    // piso de ruído: desce rápido no silêncio, sobe devagar com ruído não tonal
    if (rms < S.nf) S.nf += (rms - S.nf) * 0.2;
    else if (c < 0.6 && rms < S.nf * 3) S.nf += (rms - S.nf) * 0.02;
    if (S.nf < 0.0004) S.nf = 0.0004; else if (S.nf > 0.02) S.nf = 0.02;
    var g = gate();
    var nT = (fr.tid === S.tid && fr.p) ? S.targets.length : 0;

    // ataque: energia do último passo contra o mínimo dos três anteriores
    var mn = S.e0 < S.e1 ? S.e0 : S.e1; if (S.e2 < mn) mn = S.e2;
    if (rmsS > g * 1.3 && rmsS > mn * 1.6 + 0.0015 && t - S.onsetT > 0.05) {
      S.onsetT = t; S.fresh = true; S.hadNote = S.cur !== null; S.rmsPre = S.lastRms;
      S.cand = null; S.run = 0;
      for (k = 0; k < S.targets.length; k++) {
        S.tPre[k] = Math.max(S.h0[k], S.h1[k], S.h2[k]); S.tRun[k] = 0;
      }
      emit({ type: 'ataque', t: t, nivel: rmsS });
    }
    S.e0 = S.e1; S.e1 = S.e2; S.e2 = rmsS;
    if (S.fresh && t - S.onsetT > 0.45) S.fresh = false;

    // caminho "alvo"
    var bestK = -1, bestP = 0;
    for (k = 0; k < nT; k++) {
      var p = rms > g ? fr.p[k] : 0;
      if (S.fresh && S.onsetT >= S.shownT - 0.02 && p >= P_ON && (S.tPre[k] < 0.45 || p > S.tPre[k] + 0.12)) {
        S.tRun[k]++;
        if ((S.tRun[k] >= 2 || p >= P_FAST) && p > bestP) { bestP = p; bestK = k; }
      } else S.tRun[k] = 0;
      S.h0[k] = S.h1[k]; S.h1[k] = S.h2[k]; S.h2[k] = p;
    }
    if (bestK >= 0) {
      S.fresh = false; S.cur = S.targets[bestK]; S.cand = S.cur; S.run = 2; S.silent = 0; S.cents = 0;
      emit({ type: 'nota', midi: S.cur, cents: 0, ataque: true, via: 'alvo', t: t });
      S.lastRms = rms;
      return;
    }

    // caminho "mpm"
    var n = null, cents = 0;
    if (rms > g && f > 0 && c >= C_ON) {
      var mf = 69 + 12 * Math.log(f / 440) / Math.LN2;
      n = Math.round(mf); cents = (mf - n) * 100;
      if (n < 38 || n > 91) n = null;
    }
    // logo após um ataque sobre outra nota a janela ainda mistura as duas
    if (n !== null && S.fresh && S.hadNote && t - S.onsetT < 0.024) n = null;
    if (n === S.cand) S.run++; else { S.cand = n; S.run = 1; }
    if (n === null) {
      if (++S.silent >= 8) S.cur = null;
    } else {
      S.silent = 0;
      if (n === S.cur) S.cents = cents;
      if (S.run >= 2) {
        if (S.fresh) {
          if (n !== S.cur) {
            S.cur = n; S.cents = cents; S.fresh = false;
            emit({ type: 'nota', midi: n, cents: cents, ataque: true, via: 'mpm', t: t });
          } else if (t - S.onsetT >= 0.06 && rms >= S.rmsPre * 1.12) {
            S.fresh = false;                           // mesma nota, tocada de novo
            emit({ type: 'nota', midi: n, cents: cents, ataque: true, via: 'mpm', t: t });
          }
        } else if (n !== S.cur && S.run >= 4) {
          S.cur = n; S.cents = cents;                  // mudou sem ataque (ligado, slide)
          emit({ type: 'nota', midi: n, cents: cents, ataque: false, via: 'mpm', t: t });
        }
      }
    }
    S.lastRms = rms;
  }

  return {
    push: push, setTargets: setTargets, gate: gate, reset: reset,
    atual: function () { return S.cur; },
    cents: function () { return S.cents; },
    piso: function () { return S.nf; }
  };
}

if (typeof module !== 'undefined' && module.exports) module.exports = { makeTracker: makeTracker };
