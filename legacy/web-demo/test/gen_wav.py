# gera WAVs de teste: cordas soltas dedilhadas e uma progressão de acordes
import numpy as np, wave, sys, pathlib
sr = 48000
rng = np.random.default_rng(5)
def pluck(freq, dur, amp=0.25):
    P = sr / freq; N = int(np.floor(P - 0.6)); d = P - 0.5 - N; a = (1 - d) / (1 + d)
    buf = np.zeros(N); lp = 0.0
    for i in range(N):
        lp += 0.55 * ((rng.random() * 2 - 1) - lp); buf[i] = lp
    buf -= buf.mean()
    g = 10 ** (-3 / (freq * (3.2 if freq < 200 else 2.4)))
    n = int(sr * dur); out = np.zeros(n); p = 0; last = 0.0; pin = 0.0; pout = 0.0
    for i in range(n):
        cur = buf[p]; avg = 0.5 * (cur + last); last = cur
        ap = a * avg + pin - a * pout; pin = avg; pout = ap
        buf[p] = ap * g; out[i] = cur; p += 1
        if p == N: p = 0
    return out / np.abs(out).max() * amp
def mtof(m): return 440 * 2 ** ((m - 69) / 12)
def save(name, x):
    x = np.clip(x, -1, 1); w = wave.open(name, 'wb'); w.setnchannels(1); w.setsampwidth(2); w.setframerate(sr)
    w.writeframes((x * 32767).astype('<i2').tobytes()); w.close()
out = pathlib.Path(sys.argv[1]); out.mkdir(exist_ok=True)
# 1) seis cordas soltas, uma por segundo, com ruído de fundo
seq = [40, 45, 50, 55, 59, 64]
x = rng.normal(0, 0.002, sr * (len(seq) + 1))
for i, m in enumerate(seq):
    t = pluck(mtof(m), 0.9); o = int((0.5 + i) * sr); x[o:o + len(t)] += t * np.minimum(1, (len(t) - np.arange(len(t))) / (0.03 * sr))
save(str(out / 'soltas.wav'), x)
# 2) progressão Em–C–G–D em acordes dedilhados (para estimar tom): 3 voltas
chords = [[40, 47, 52, 55, 59, 64], [48, 52, 55, 60, 64], [43, 47, 50, 55, 59, 67], [50, 57, 62, 66]]
y = np.zeros(sr * 2 * len(chords) * 3 + sr)
k = 0
for rep in range(3):
    for ch in chords:
        for j, m in enumerate(ch):
            for beat in range(2):
                t = pluck(mtof(m), 1.0, 0.12); o = int((k * 2 + beat + j * 0.03) * sr); y[o:o + len(t)] += t
        k += 1
save(str(out / 'prog_em.wav'), y)
print('wavs ok')
