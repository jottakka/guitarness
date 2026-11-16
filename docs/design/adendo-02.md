# Guitarness PC (Guitarness) — Adendo 02 ao DESIGN v1

Data: 16/11/2026. Este adendo **acrescenta** ao DESIGN v1 e ao Adendo 01. Onde houver conflito sobre tempo, direção, portões ou fita, vale este texto. Aqui ficam as regras de desenho.

Estado da evidência: toda a evidência de tempo é sintética (`FakeBackend`, jogador virtual do `guitarness-sim`). Nenhuma regra abaixo foi provada com microfone real. As fitas reais do roteiro (`docs/roteiro-primeira-sessao.md`) são o próximo passo.

## B1. Tempo: sistema e jogador são duas coisas (P8)

Regra. O juiz separa a latência do **sistema** da tendência do **jogador**. Nunca subtrai a tendência.

- A calibração mede o laço acústico (alto-falante para microfone) e dele tira `L_rt`. O juiz subtrai só `L_rt` (invariante I4). A média do jogador **não** vira a batida.
- Com laço medido, o juiz julga **qualquer** espalhamento do jogador. Um jogador espalhado passa a ser julgável. O M2 dizia "ainda não".
- Sem laço (fone, Bluetooth), latência e tendência são inseparáveis. O jogo usa uma estimativa (mediana dos golpes abafados do passo de batida), julga só se o espalhamento for pequeno (MAD ≤ 15 ms) e **nunca afirma tendência**.
- Sem calibração, ou com perfil de outra fonte de carimbo: `NotJudged(NoCalibration)`. O jogo só mostra "ouvi".
- A tendência só aparece com laço, pelo menos 12 golpes e incerteza ≤ 15 ms. Senão o jogo não afirma nada (`NotClaimed`). "No centro" só vale com laço de σ ≤ 8 ms.
- A calibração fica congelada durante a rodada (I5). A verificação rápida roda só entre rodadas.
- Janela. A rodada começa no Aprendiz (±120 ms). A sugestão de janela mais estreita é uma **oferta** (chip). Nada estreita sozinho.

Teste que prova a regra (F4): dois mundos no `FakeBackend`, (L = 87,3 ms, tendência −30 ms) e (L = 57,3 ms, tendência 0). Com laço, o jogo mede `L_rt` certo nos dois e afirma tendência só no primeiro. Sem laço, os dois dão a mesma estimativa e nenhum afirma tendência.

## B2. Passagem de som: o chirp curto (resposta à pergunta de 3 s)

Decisão: **sim, onde existe laço acústico**, com duas condições.

1. A "Passagem curta" é o caminho padrão com laço: 1,0 s de silêncio, 6 sondas a 300 ± 40 ms, análise. O jogador não faz nada. Mede `L_rt`, saturação e microfone bloqueado, e dá a dica de Bluetooth (gentil, nunca erro).
2. O passo de 16 batidas sai do caminho obrigatório quando o laço foi medido. A tendência é observada na primeira rodada, que já é jogo.

O que o chirp não substitui:

- Sem laço, fica o passo de batida (~12 s), base `Estimate`, sem tendência.
- Recusa da rodada curta cai sozinha na rodada completa de 10 sondas, depois na de −6 dBFS, depois no passo de batida. Nunca há tela de falha.
- `guitarness calibrate` (CLI) mantém as 10 sondas. É a ferramenta de repetibilidade em hardware.

Limite honesto: a taxa de recusa da rodada curta em sala real é desconhecida.

## B3. Direção: `NotJudged`

O Pulso **não julga direção**. Faltam evidências: o início por corda (átomos do C2) não existe; `Strum::spread` é só dica e não tem sinal; golpe abafado não ordena cordas; a literatura dá 80–90% em microfone, insuficiente para reprovar; o padrão alternado nunca conta como evidência.

Regra de desenho. A seta D/U aparece como guia. No código, `Direction` tem uma única variante, `NotJudged(Why)`: o juiz não consegue acusar direção, nem por engano. O portão `Direction` nasce `Locked`, com o motivo escrito.

## B4. Portões por tipo de veredito

Cada tipo de veredito só julga quando a evidência o autoriza.

| Estado | Efeito | Como chega |
|---|---|---|
| `Locked` | `NotJudged(GateLocked)` | padrão de todo tipo sem evidência |
| `Provisional` | julga | evidência só sintética, medida no CI |
| `Open` | julga | fitas **reais** que passam a regra |

- Regra padrão: n ≥ 200 eventos rotulados e `false_wrong / n ≤ 1/200`. Para `Open`, em pelo menos 3 fitas reais.
- Padrão compilado: `Attack`, `Timing`, `Tendency` = `Provisional`. `Direction`, `Accent`, `Muted`, `ChordIdentity`, `StringMissing`, `StringExtra`, `WrongFret` = `Locked`.
- O relatório mostra o limite superior de Wilson a 95%. Com 200 eventos e 0 falsos, o limite é 1,5%. O número do voto é estimativa pontual, e o relatório diz isso.
- `gates.json` na pasta de dados sobrepõe o padrão. `guitarness replay --gates` o gera.
- Só fita real abre `Open`. Evidência sintética nunca abre.
- O portão tem de poder fechar: o teste `gates_synthetic` tem controle negativo (um deslocamento de +40 ms não passa).

## B5. Fita: toda sessão deixa evidência

- Uma fita é uma época do motor, numa pasta com `tape.json`, `mic.wav` (mono `f32`), `mic.runs.json` e `events.jsonl`. Reabrir o dispositivo fecha a fita e abre outra.
- O jogo julga como função pura da sequência de eventos do motor e dos comandos da sessão (I2). Por isso o replay é exato. O juiz não lê relógio de parede.
- A saída do jogo na fita é a **agenda**, não um segundo WAV. O mixer é determinístico.
- Fita incompleta (queda, perda do anel cru, dano no arquivo) fica fora dos portões, com o nome na lista de excluídas.
- Contestar (Backspace) nunca concede acerto. O veredito sai da conta, a fita marca a contestação e a fita contestada nunca é apagada.
- Áudio da sala fica só no disco local. O jogador vê o indicador de fita e liga ou desliga com a tecla R. Padrão atual: fita **desligada** até a decisão do responsável (opt-in com `--tape`).

## B6. Regras do jogador que este adendo mantém

Sem punição e sem tela de falha. Erro em branco ou cinza, mais forma (saturação 0). Golpe nunca é opcional (sem ataque não há acerto; só o fantasma é exceção, explícita). Só se julga o que tem evidência. Não se afirma o que não se mediu. Ajuste de andamento e janela só por oferta. LLM fora do tempo real. Arte sem texto gerado; texto só no painel Detalhes.

## B7. Princípio do P5 para o M6: a Estrada mostra ritmo, não seis cordas

Regra para o M6 (vistas de jogo): a vista padrão do ritmo é **um glifo por evento na linha de acerto**, sem painel de seis cordas. O jogador lê tempo e tipo de golpe, não posição de dedo. O painel de cordas é opcional e vira só uma vista alternativa (tecla V, que já existe). Motivo: no treino de tempo, a atenção é do ritmo; seis cordas competem com ela e o juiz atual não julga corda nem direção (B3).

## B8. Itens que o responsável decide

Decisões em aberto (gravar toda sessão por padrão, repetir o passo de batida sem laço, texto sob pictogramas, andamento automático, janela inicial, tecla de contestar, crate `guitarness-session`, pasta de dados, licença do jogo, portão do tempo `Provisional`). Até a resposta, valem os padrões do [relatório da fatia Pulso](../relatorios/fatia-pulso.md).
