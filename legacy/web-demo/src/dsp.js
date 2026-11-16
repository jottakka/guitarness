/* =====================================================================
   DSP — tudo aqui é função pura (roda no navegador, no AudioWorklet e no Node)
   ===================================================================== */

/* Detector de altura para violão.
   - analyze(): McLeod Pitch Method (NSDF + 1º pico relevante) para nota única.
   - presence(f): "a nota esperada está soando?" — tolera cordas que ainda
     estão vibrando, porque olha direto o pico da NSDF no período do alvo.
   fs = taxa de amostragem já decimada; W = tamanho da janela (amostras). */
function makeDetector(fs, W) {
  var K = 0.9;                                    // 1º pico >= K * maior pico
  var minLag = Math.max(2, Math.floor(fs / 1450));
  var maxLag = Math.min((W >> 1) - 2, Math.ceil(fs / 68));
  var L = maxLag + 1;                             // calculamos nsdf[0..L]
  var nsdf = new Float32Array(L + 2);
  var kLag = new Int32Array(96), kVal = new Float32Array(96);
  var pkLag = 0, pkVal = 0;
  var res = { f: 0, c: 0, rms: 0, rmsS: 0 };

  // interpolação parabólica em torno de um máximo local inteiro
  function refine(i) {
    var a = nsdf[i - 1], b = nsdf[i], c = nsdf[i + 1];
    var d = a - 2 * b + c, sh = 0;
    if (d < 0) { sh = 0.5 * (a - c) / d; if (sh > 0.5) sh = 0.5; else if (sh < -0.5) sh = -0.5; }
    pkLag = i + sh; pkVal = b - 0.25 * (a - c) * sh;
  }

  function analyze(x, hopN) {
    var j, tau, n, acf, m = 0, s = 0;
    for (j = 0; j < W; j++) m += x[j] * x[j];
    res.rms = Math.sqrt(m / W);
    if (hopN > W) hopN = W;
    for (j = W - hopN; j < W; j++) s += x[j] * x[j];
    res.rmsS = Math.sqrt(s / hopN);

    // NSDF: n(tau) = 2 r(tau) / m(tau), com m(tau) atualizado em O(1)
    var mm = 2 * m;
    for (tau = 0; tau <= L; tau++) {
      acf = 0; n = W - tau;
      for (j = 0; j < n; j++) acf += x[j] * x[j + tau];
      nsdf[tau] = mm > 1e-9 ? 2 * acf / mm : 0;
      var a = x[tau], b = x[W - 1 - tau];
      mm -= a * a + b * b;
    }
    nsdf[L + 1] = nsdf[L];

    // máximos-chave: o maior máximo local de cada lobo positivo
    var i = 1, nK = 0, best = 0;
    while (i < L && nsdf[i] > 0) i++;             // pula o lobo em torno de tau = 0
    while (i < L) {
      while (i < L && nsdf[i] <= 0) i++;
      var mi = -1, mv = 0;
      while (i < L && nsdf[i] > 0) {
        if (nsdf[i] > mv && nsdf[i] >= nsdf[i - 1] && nsdf[i] >= nsdf[i + 1]) { mv = nsdf[i]; mi = i; }
        i++;
      }
      if (mi >= minLag && mi <= maxLag && nK < 96) { kLag[nK] = mi; kVal[nK] = mv; nK++; if (mv > best) best = mv; }
    }
    res.f = 0; res.c = 0;
    if (nK && best > 0.3) {
      var th = best * K;
      for (var k = 0; k < nK; k++) {
        if (kVal[k] >= th) { refine(kLag[k]); res.f = fs / pkLag; res.c = pkVal; break; }
      }
    }
    return res;
  }

  // altura do pico da NSDF a até ±45 cents do atraso t0 (0 se não há pico ali)
  function peakNear(t0) {
    var lo = Math.floor(t0 * 0.974), hi = Math.ceil(t0 * 1.026);
    if (lo < 1) lo = 1;
    if (hi > L - 1) hi = L - 1;
    var bi = -1, bv = 0;
    for (var i = lo; i <= hi; i++) {
      var v = nsdf[i];
      if (v > bv && v >= nsdf[i - 1] && v >= nsdf[i + 1]) { bv = v; bi = i; }
    }
    if (bi < 0) return 0;
    refine(bi);
    var cents = 1200 * Math.log(pkLag / t0) / Math.LN2;
    return (cents <= 45 && cents >= -45) ? pkVal : 0;
  }

  // 0..1: quão presente está a nota de frequência freq na última janela analisada
  function presence(freq) {
    var t0 = fs / freq;
    if (t0 > maxLag || t0 < minLag) return 0;
    var s1 = peakNear(t0);
    if (s1 < 0.5) return 0;
    // se metade ou um terço do período explicam tão bem, a nota real é mais aguda
    if (t0 / 2 >= minLag && peakNear(t0 / 2) > s1 - 0.05) return 0;
    if (t0 / 3 >= minLag && peakNear(t0 / 3) > s1 - 0.05) return 0;
    // se o dobro do período explica bem melhor, a nota real é a oitava de baixo
    if (2 * t0 <= maxLag && peakNear(2 * t0) > s1 + 0.12) return 0;
    return s1;
  }

  return { analyze: analyze, presence: presence, minLag: minLag, maxLag: maxLag };
}

/* Corda dedilhada sintética (Karplus-Strong com afinação fina por passa-tudo).
   Usada no modo toque e nos testes. */
function ksPluck(sr, freq, dur, rnd) {
  rnd = rnd || Math.random;
  var P = sr / freq, N = Math.floor(P - 0.6), d = P - 0.5 - N;
  var a = (1 - d) / (1 + d);
  var len = Math.floor(sr * dur), out = new Float32Array(len), buf = new Float32Array(N);
  var i, lp = 0, m = 0;
  for (i = 0; i < N; i++) { lp += 0.55 * ((rnd() * 2 - 1) - lp); buf[i] = lp; m += lp; }
  m /= N;
  for (i = 0; i < N; i++) buf[i] -= m;
  var g = Math.pow(10, -3 / (freq * (freq < 200 ? 3.2 : 2.4)));   // decaimento por período
  var p = 0, last = 0, pin = 0, pout = 0, pk = 0;
  for (i = 0; i < len; i++) {
    var cur = buf[p];
    var avg = 0.5 * (cur + last); last = cur;
    var ap = a * avg + pin - a * pout; pin = avg; pout = ap;
    buf[p] = ap * g;
    out[i] = cur;
    if (cur > pk) pk = cur; else if (-cur > pk) pk = -cur;
    if (++p === N) p = 0;
  }
  if (pk > 0) for (i = 0; i < len; i++) out[i] /= pk;
  return out;
}

/* FFT radix-2 in-place */
function fft(re, im) {
  var n = re.length, i, j, k, t;
  for (i = 1, j = 0; i < n; i++) {
    var bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) { t = re[i]; re[i] = re[j]; re[j] = t; t = im[i]; im[i] = im[j]; im[j] = t; }
  }
  for (var len = 2; len <= n; len <<= 1) {
    var ang = -2 * Math.PI / len, wr = Math.cos(ang), wi = Math.sin(ang), h = len >> 1;
    for (i = 0; i < n; i += len) {
      var cr = 1, ci = 0;
      for (k = 0; k < h; k++) {
        var ur = re[i + k], ui = im[i + k];
        var vr = re[i + k + h] * cr - im[i + k + h] * ci;
        var vi = re[i + k + h] * ci + im[i + k + h] * cr;
        re[i + k] = ur + vr; im[i + k] = ui + vi;
        re[i + k + h] = ur - vr; im[i + k + h] = ui - vi;
        t = cr * wr - ci * wi; ci = cr * wi + ci * wr; cr = t;
      }
    }
  }
}

/* Tom provável de uma gravação: cromagrama + perfis de tonalidade.
   mono: Float32Array; sr: taxa. Devolve {tonica 0..11, menor, confianca, croma}.
   opt (só para experimentos, ver test/tom.js): { perfil: 'kk' | 'temperley' | 'shaath', explica: bool } */
var PERFIS_TOM = {
  kk: [[6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88], [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17]],
  temperley: [[0.748, 0.060, 0.488, 0.082, 0.670, 0.460, 0.096, 0.715, 0.104, 0.366, 0.057, 0.400], [0.712, 0.084, 0.474, 0.618, 0.049, 0.460, 0.105, 0.747, 0.404, 0.067, 0.133, 0.330]],
  shaath: [[6.6, 2.0, 3.5, 2.3, 4.6, 4.0, 2.5, 5.2, 2.4, 3.7, 2.3, 3.4], [6.5, 2.7, 3.5, 5.4, 2.6, 3.5, 2.5, 5.2, 4.0, 2.7, 4.3, 3.2]]
};
function detectKey(mono, sr, opt) {
  opt = opt || {};
  var perfil = PERFIS_TOM[opt.perfil || 'temperley'], explica = opt.explica === true;
  var D = Math.max(1, Math.round(sr / 11025)), fsd = sr / D, N = 4096;
  var total = Math.floor(mono.length / D);
  if (total < N * 2) return null;
  var frames = Math.min(320, Math.floor(total / N));
  var re = new Float32Array(N), im = new Float32Array(N), win = new Float32Array(N);
  var i, k, b;
  for (i = 0; i < N; i++) win[i] = 0.5 - 0.5 * Math.cos(2 * Math.PI * i / (N - 1));
  var bLo = Math.ceil(100 * N / fsd), bHi = Math.min(N / 2 - 2, Math.floor(1600 * N / fsd)), bMin = Math.ceil(45 * N / fsd);
  var pcOf = new Int8Array(bHi + 1), wOf = new Float32Array(bHi + 1), mag = new Float32Array(bHi + 2);
  for (b = bLo; b <= bHi; b++) {
    var pitch = 69 + 12 * Math.log(b * fsd / N / 440) / Math.LN2, r = Math.round(pitch);
    pcOf[b] = ((r % 12) + 12) % 12;
    wOf[b] = 1 - 2 * Math.abs(pitch - r);
  }
  var chroma = new Float64Array(12), fc = new Float64Array(12);
  for (k = 0; k < frames; k++) {
    var start = Math.floor(k * (total - N) / Math.max(1, frames - 1));
    for (i = 0; i < N; i++) {
      var acc = 0, base = (start + i) * D;
      for (var q = 0; q < D; q++) acc += mono[base + q];
      re[i] = acc / D * win[i]; im[i] = 0;
    }
    fft(re, im);
    for (b = 0; b <= bHi + 1; b++) mag[b] = Math.sqrt(re[b] * re[b] + im[b] * im[b]);
    var sum = 0;
    for (i = 0; i < 12; i++) fc[i] = 0;
    for (b = bLo; b <= bHi; b++) {
      var m = mag[b];
      if (explica) {
        // um pico que tem parcial forte em f/3 ou f/5 é quase sempre harmônico de outra nota
        // (a quinta e a terça "fantasmas" que puxam o tom para o vizinho errado)
        var b3 = Math.round(b / 3), b5 = Math.round(b / 5);
        if (b3 >= bMin && Math.max(mag[b3 - 1], mag[b3], mag[b3 + 1]) > 0.5 * m) m *= 0.25;
        else if (b5 >= bMin && Math.max(mag[b5 - 1], mag[b5], mag[b5 + 1]) > 0.5 * m) m *= 0.4;
      }
      m *= wOf[b];
      fc[pcOf[b]] += m; sum += m;
    }
    if (sum > 1e-6) for (i = 0; i < 12; i++) chroma[i] += fc[i] / sum;
  }
  function corr(prof, tonic) {
    var mx = 0, my = 0, sxy = 0, sxx = 0, syy = 0, j;
    for (j = 0; j < 12; j++) { mx += chroma[(tonic + j) % 12]; my += prof[j]; }
    mx /= 12; my /= 12;
    for (j = 0; j < 12; j++) {
      var dx = chroma[(tonic + j) % 12] - mx, dy = prof[j] - my;
      sxy += dx * dy; sxx += dx * dx; syy += dy * dy;
    }
    return sxx > 0 ? sxy / Math.sqrt(sxx * syy) : 0;
  }
  var best = { tonica: 0, menor: false, r: -2 }, second = -2;
  for (var t = 0; t < 12; t++) {
    for (var md = 0; md < 2; md++) {
      var c = corr(perfil[md], t);
      if (c > best.r) { second = best.r; best = { tonica: t, menor: !!md, r: c }; }
      else if (c > second) second = c;
    }
  }
  return { tonica: best.tonica, menor: best.menor, confianca: best.r - second, r: best.r, croma: Array.prototype.slice.call(chroma) };
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { makeDetector: makeDetector, ksPluck: ksPluck, fft: fft, detectKey: detectKey };
}
