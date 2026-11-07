---
name: release
description: Publish a new Duck release - bump the version on develop, wait for CI, merge develop into main and follow the Release workflow until the Linux and Windows packages are on GitHub. Use when the user asks to release, publish, ship or cut a version of Duck ("gerar release", "publicar versao", "/release").
---

# Release do Duck

Fluxo de branches:
- `develop`: todo o trabalho do dia a dia. Cada push roda o `CI` (`.github/workflows/ci.yml`): testes e build das extensões no Linux e no Windows.
- `main`: só recebe merge da `develop`. Cada push roda o `Release` (`.github/workflows/release.yml`). Ele lê a versão do `Cargo.toml`, compila e publica a tag `vX.Y.Z` com os pacotes `duck-linux-x86_64.tar.gz`, `duck-windows-x86_64.zip` e `SHA256SUMS`.
- Uma versão que já tem tag faz o Release falhar de propósito. Por isso a versão sobe antes do merge.

Argumento opcional: `patch`, `minor`, `major` ou uma versão exata (`0.2.0`).

## Passos

1. **Conferir o ponto de partida**
   ```sh
   git fetch --tags origin
   git status -sb          # tem que estar na develop, limpo
   git pull --ff-only
   ```
   Se houver mudanças não commitadas, pare e pergunte ao usuário o que fazer. Não faça stash nem descarte nada.

2. **Escolher a versão**
   - Veja o que entra: `git log --oneline $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD`. Sem tag nenhuma, é o primeiro release.
   - Se não veio argumento, proponha a versão:
     - `patch` para correções.
     - `minor` para funcionalidades novas.
     - `major` para algo que quebra config ou protocolo.
   - Confirme a versão com o usuário antes de seguir.
   - No primeiro release, a versão atual do `Cargo.toml` serve como está, se ainda não tiver tag.

3. **Subir a versão** (pule se a versão já for a do `Cargo.toml`)
   - Edite `version = "..."` no `[package]` do `Cargo.toml`.
   - Edite a entrada `name = "duck"` do `Cargo.lock` (a linha `version` logo abaixo). O CI usa `--locked`; se o lock ficar diferente, ele falha.
   - Para conferir: `cargo metadata --locked --format-version 1 >/dev/null`. Neste Fedora, cargo só roda no container: `~/containers/bin/dev rust cargo metadata --locked --format-version 1 >/dev/null`.
   - Commit `Release vX.Y.Z`, com a linha de co-autoria de sempre.

4. **Push da develop e esperar o CI**
   ```sh
   git push origin develop
   gh run list --workflow CI --branch develop --limit 1     # pegue o id
   gh run watch <id> --exit-status
   ```
   Se o CI falhar, mostre o erro (`gh run view <id> --log-failed | tail -50`) e pare. Não faça o merge.

5. **Merge na main**
   ```sh
   git switch main && git pull --ff-only
   git merge --ff-only develop
   git push origin main
   git switch develop
   ```
   Se o `--ff-only` falhar, a main tem commits que a develop não tem (um hotfix, por exemplo). Nesse caso:
   - Volte para a develop.
   - Rode `git merge main`, resolva, faça o push e espere o CI de novo.
   - Repita este passo.

6. **Acompanhar o Release**
   ```sh
   gh run list --workflow Release --branch main --limit 1
   gh run watch <id> --exit-status
   gh release view vX.Y.Z --json url,assets --jq '.url, (.assets[].name)'
   ```
   Se falhar, mostre o log do passo que quebrou. Não apague tags nem releases sem perguntar.

7. **Relatório ao usuário**
   - Versão publicada e link do release.
   - Os três arquivos publicados.
   - Os comandos de instalação:
     - `curl -fsSL https://raw.githubusercontent.com/herlondf/duck/main/get.sh | sh`
     - `irm https://raw.githubusercontent.com/herlondf/duck/main/get.ps1 | iex`

## Não fazer

- Push direto na `main` de algo que não veio da `develop`.
- Pular o CI.
- Reusar um número de versão.
- Usar `git push --force` em `main` ou `develop`.
- Criar a tag na mão: quem cria é o workflow, junto com o release.
