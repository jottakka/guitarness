# Relatório M0 + M1 (16/11/2026)

## Verificado

| Afirmação | Evidência |
|---|---|
| Workspace compila em Windows e macOS | `cargo build --release --workspace` em Windows (rustc 1.100.0, MSVC) e em macOS (rustc 1.100.0, aarch64) |
| Paridade com a demo nos 5 cenários golden, autocorrelação direta | `cargo test --release -p tonica-dsp --test golden`: 0 quadros fora da tolerância, mesmos eventos (tipo, nota, via, tempo ±1e-5 s), nos dois sistemas |
| Mesmo resultado com autocorrelação por FFT | mesmo teste, `golden_fft_events`: eventos idênticos, 0 quadros fora da tolerância |
| Placar igual ao `RESUMO.txt` | `golden_scores_match_resumo`: A 37/37, B 23/23, C 22/23, D 20/23, F 9/9; nenhum evento via alvo no F |
| Caminho por bloco sem alocação | `tests/no_alloc.rs` com `assert_no_alloc` (debug). Controle negativo: um `vec!` dentro da guarda aborta o processo (SIGABRT) |
| fmt e clippy limpos | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings` |

Desvio máximo por quadro (igual nos dois back-ends): Δf 5,0e-5 Hz, Δc 5,0e-6, Δrms relativo 4,9e-7, Δp 5,0e-6. Isso é o arredondamento do próprio CSV (4 a 6 casas), não diferença de cálculo.

Tolerância declarada (`crates/tonica-dsp/tests/golden.rs`): t 2e-6 s; f 5e-3 Hz (zero e não-zero precisam concordar); c 2e-4; rms 1e-5 relativo; presença 2e-4.

## Custo por quadro (`cargo bench -p tonica-dsp --bench frame`)

| Máquina | Direta (fiel) | FFT | FFT + 3 presenças |
|---|---|---|---|
| Desktop Windows (x86-64) | 221 µs | **7,2 µs** | **7,4 µs** |
| Mac Apple silicon | 140 µs | **6,2 µs** | **6,2 µs** |
| Demo em JavaScript (Node) | ~270 µs | — | — |

Meta: até 30 µs por quadro em desktop. **Cumprida com a FFT**, sem SIMD manual ainda.

## Decisões

- Dois back-ends de autocorrelação: `Direct` é a referência bit a bit da demo; `Fft` é o de produção. O teste golden roda os dois.
- Limiares do rastreador viram `TrackerParams::for_hop`, que converte contagens de quadro para tempo quando o passo muda.

## Não testado

- Microfone ao vivo, latência de ponta a ponta, teste de dez minutos (M2).
- Violão real, GuitarSet, GAPS.
- Porte do `sim.js` (241 notas) e das variantes de taxa (`SR=44100`, `SR=16000`).
- Decimação com FIR polifásico (o porte ainda usa a média de duas amostras da demo, por paridade).
- O ponto fraco forte/fraco continua igual à demo (20/23 no golden), porque é um porte fiel.
