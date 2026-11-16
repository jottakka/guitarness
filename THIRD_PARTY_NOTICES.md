# Avisos de terceiros

O código do Guitarness usa a [licença MIT](LICENSE). Este arquivo lista o que tem outra licença ou outra origem.

## Fonte Fira Mono

- Arquivo: `crates/guitarness-game/fonts/FiraMono-Medium.ttf`, embutido no jogo.
- Licença: SIL Open Font License 1.1 (OFL 1.1).
- Texto da licença: [`LICENSES/FiraMono-OFL-1.1.txt`](LICENSES/FiraMono-OFL-1.1.txt). Uma cópia fica ao lado da fonte, em `crates/guitarness-game/fonts/FiraMono-LICENSE`.
- Direitos: dados digitalizados, copyright (c) 2012-2015, The Mozilla Foundation e Telefonica S.A.

A fonte continua sob a OFL, e não sob a licença MIT do restante do projeto.

## audio_thread_priority (MPL-2.0)

- Crate `audio_thread_priority` 0.35.1, trazido como dependência indireta do `cpal` (recurso `realtime`). Aparece nos dois `Cargo.lock`.
- Licença: Mozilla Public License 2.0, com copyleft de arquivo.
- O projeto não modifica o crate. Quem o redistribuir com modificações nos arquivos cobertos pela MPL-2.0 precisa publicar essas modificações sob a mesma licença.

## r-efi (escolha de licença)

- Crates `r-efi` 5.3.0 e 6.0.0, presentes só no `Cargo.lock` do jogo (`crates/guitarness-game`).
- Licença oferecida: `MIT OR Apache-2.0 OR LGPL-2.1-or-later`. Para este projeto vale a escolha **MIT** (ou Apache-2.0), e não a LGPL.

## Imagens geradas por IA

As imagens em [`docs/arte/minimal/`](docs/arte/minimal/) foram geradas com o gerador de imagens da OpenAI (ChatGPT / gpt-image). Os arquivos PNG mantêm os metadados C2PA de proveniência, que indicam mídia gerada por algoritmo e o software usado. Eles não devem ser removidos.

- Os prompts exatos estão no [README da pasta](docs/arte/minimal/README.md).
- Não há concessão de licença explícita sobre essas imagens além do que a ferramenta e seus termos permitem. Não afirme autoria humana exclusiva sobre elas.
- São arte conceitual de referência, não arte final do jogo.

As capturas de tela em `crates/guitarness-game/shots/` são geradas pelo próprio projeto.

## Demais dependências

As dependências de terceiros do código Rust são resolvidas pelo Cargo. Suas versões exatas e origens estão em [`Cargo.lock`](Cargo.lock) e em [`crates/guitarness-game/Cargo.lock`](crates/guitarness-game/Cargo.lock). Cada uma mantém a sua própria licença, declarada no manifesto do crate. Este arquivo não substitui uma auditoria de licenças com `cargo-deny` ou `cargo-about`, que ainda não foi feita.
