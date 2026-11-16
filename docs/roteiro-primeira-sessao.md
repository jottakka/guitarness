# Roteiro da primeira sessão com microfone real

Quem roda: **a pessoa responsável pelo projeto**. Duração: cerca de 30 minutos. Este roteiro gera as fitas reais que abrem os portões e alimentam o teste do juiz com gravações reais (veja a [pesquisa de modelos de detecção](pesquisa/modelos-deteccao.md)).

Aviso: os passos 2, 3 e 5 tocam bipes e cliques. Combine o horário.

## Antes de começar

- Use um Mac ou um PC Windows com microfone e caixa de som. O sistema de som tem de ouvir o próprio alto-falante: caixa de som na mesa, microfone aberto. Fone ou Bluetooth não têm laço; o jogo usa então só uma estimativa e não afirma tendência (os comandos funcionam, o resultado é mais pobre).
- Desligue o ganho automático e os efeitos de áudio do microfone se o sistema oferecer (no Windows: "melhorias de áudio" desligadas).
- Aceite o pedido de microfone do macOS uma vez. Se o jogo rodar do terminal, a permissão vale para o terminal.
- Pasta de trabalho: escolha uma pasta de dados, por exemplo `./guitarness-data`. Os comandos abaixo usam `$DADOS` para ela.
- Binários: `guitarness` é o da CLI (`crates/guitarness-cli`); o jogo é `guitarness-game` (`crates/guitarness-game`, workspace próprio). Compile em release antes: `cargo build --release` na raiz e `cargo build --release --manifest-path crates/guitarness-game/Cargo.toml`.

```
export DADOS=./guitarness-data
mkdir -p "$DADOS"
```

## Passo 1 — dispositivos (sem som)

```
guitarness audio devices
```

Anote os ids da entrada e da saída que você vai usar (`--in ID --out ID` nos comandos seguintes; sem eles vale o padrão do sistema). Nada toca.

## Passo 2 — sonda (bipes baixos)

```
guitarness audio probe --beep
```

Toca um clique baixo (−20 dBFS) a cada 0,5 s por 3 s. Se o comando sair com código diferente de 0, anote o código e a mensagem. Códigos: 10 sem entrada, 11 sem saída, 12 permissão ou fluxo, 13 formato.

## Passo 3 — calibração (bipes)

```
guitarness calibrate --beep --repeat 3 --profile "$DADOS/perfil.json"
```

Fique em silêncio, sem tocar violão. A calibração roda 3 vezes; o relatório mostra a faixa de `L_rt`. Para o relatório com números: acrescente `--detalhes`. Para a verificação curta (~4 s): `--short`.

Anote: o texto final, o código de saída e a faixa de `L_rt`.

## Passo 4 — afinador (sem som)

```
guitarness tune --seconds 10
```

Toque cada corda solta. Ele escuta; não toca nada. Anote a corda e o desvio em cents que ele mostrou, se estiver errado de forma óbvia.

## Passo 5 — jogo no Pulso, 5 minutos

```
cargo run --release --manifest-path crates/guitarness-game/Cargo.toml -- --pulse --live --tape --data "$DADOS"
```

O jogo abre uma janela. Passagem de som automática (cerca de 3 s, sem tocar nada), depois a contagem e a rodada.

Teclas: Espaço ou Enter começa; V alterna vista; R liga ou desliga a fita; D abre os Detalhes; Enter aceita uma oferta; Esc sai.

Durante a rodada:

1. Toque a palhetada pedida, junto com o clique. Jogue 5 minutos, em várias rodadas.
2. **Contestar.** Quando um veredito parecer errado, aperte **Backspace**. Exemplos: o jogo disse "não ouvi" e você tocou; o jogo contou golpe que você não deu. O jogo mostra "?" e depois "saiu da conta". Contestar nunca dá acerto; só tira o veredito da conta e marca a fita.
3. **Acerto da tela.** Se o aro de acerto chega antes ou depois do clique que você ouve, ajuste com `[` e `]` (±5 ms por toque) até os dois coincidirem. Anote o valor final. Ele fica na fita (`video_offset_ms`).
4. Se o jogo mostrar o ponto da fita apagado ou a Passagem de som sem resultado, aperte R ou T (tentar de novo) uma vez e anote o que aconteceu.

Opção para repetir só o acerto da tela: `--video-offset MS` na linha do comando.

## Passo 6 — tomadas de referência (10 minutos)

Cada comando grava uma fita de 10 s com um rótulo. Rode com o violão no colo, o microfone onde você joga.

```
guitarness audio record "$DADOS/tapes" --take silencio       --seconds 10
guitarness audio record "$DADOS/tapes" --take so-cliques     --seconds 10 --click 90 --beep
guitarness audio record "$DADOS/tapes" --take fala           --seconds 10
guitarness audio record "$DADOS/tapes" --take acorde-solto   --seconds 10
guitarness audio record "$DADOS/tapes" --take martelada      --seconds 10
guitarness audio record "$DADOS/tapes" --take acorde-errado  --seconds 10 --click 90 --beep
guitarness audio record "$DADOS/tapes" --take base-sem-violao --seconds 10 --click 90 --beep
guitarness audio record "$DADOS/tapes" --take abafado-nailon --seconds 10 --click 90 --beep
```

O que fazer em cada uma:

| Rótulo | O que você faz | Por quê |
|---|---|---|
| `silencio` | nada; fique quieto | todo ataque detectado é falso |
| `so-cliques` | nada; só o clique toca | clique vazado não pode virar ataque |
| `fala` | fale ou converse perto do microfone | todo ataque detectado é falso |
| `acorde-solto` | toque um acorde uma vez e deixe soar | acorde soando não é ataque novo |
| `martelada` | martele uma nota firme, sem palhetar | o detector conta martelada como golpe; queremos medir |
| `acorde-errado` | toque de propósito o acorde errado, no clique | evidência para `ChordIdentity` (hoje `Locked`) |
| `base-sem-violao` | só a base (clique) toca, sem violão | falsos ataques com base tocando |
| `abafado-nailon` | golpes abafados em cordas de náilon, se tiver | pouca energia em 2–5 kHz é risco conhecido |

Atenção: sem `--chart`, as tomadas não geram veredito para quem tocou; elas só guardam o que o jogo ouviu. Rótulos `silencio`, `so-cliques` e `fala` contam como "todo ataque é falso" nos portões. Os outros rótulos servem de matéria-prima para o teste do juiz com gravações reais.

Sem `--beep` o clique fica mudo. A fita da tomada tem o perfil só se você passar `--profile "$DADOS/perfil.json"`; passe-o nos comandos que tocam (`--click`).

## Passo 7 — conferir as fitas (sem som)

```
guitarness replay "$DADOS/tapes" --report
guitarness replay "$DADOS/tapes" --disputes
```

`--report` mostra a tabela de confusão. `--disputes` lista o que você contestou. Para guardar o áudio de uma contestação (1 s antes e depois):

```
guitarness replay "$DADOS/tapes" --export-dispute 1 contestacao-1.wav
```

Código de saída 30 quer dizer diferença no replay ou portão que reprova; 20, fita ilegível. Anote o código.

## O que mandar de volta

1. A pasta `$DADOS/tapes/` inteira. As fitas contestadas nunca são apagadas; as outras ficam de pé porque a limpeza automática nunca roda sozinha.
2. O arquivo `$DADOS/perfil.json`.
3. Um bilhete com:
   - sistema e máquina (macOS ou Windows), dispositivo de entrada e saída, caixa ou fone, e se havia ruído de fundo;
   - o valor final de `[` e `]` (ajuste de tela, em ms);
   - para cada passo que falhou: o código de saída e a mensagem;
   - quantas vezes você contestou e em que situações;
   - qualquer coisa que o jogo disse "não ouvi" e você tocou, e qualquer golpe contado que você não deu.
4. Respostas aos itens em aberto do [adendo 02](design/adendo-02.md) (B8) que você já souber: gravar toda sessão por padrão, repetir o passo de batida sem laço, texto sob os pictogramas, Backspace para contestar.

## Cuidados

- As fitas guardam o áudio da sala em `f32` mono: cerca de 11,5 MB por minuto a 48 kHz. Ficam só no disco local. Apague o que não quiser enviar; não apague as contestadas.
- Se o jogo ou um comando travar, feche a janela e anote a mensagem. Não repita mais de duas vezes o mesmo passo.
- Fones e Bluetooth: rode o roteiro de novo com eles depois, e anote que são fones.
