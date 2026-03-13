# Continuous Integration

To utilize continuous integration for your Buffrs package (e.g. to automate code review and publishing
of your packages) you can utilize the following templates for GitHub Actions and GitLab CI:

## GitHub Actions

```yaml
name: Buffrs

on:
  push:
    branches:
      - "*"
  tags:
    - "*"

env:
  BUFFRS_VERSION: 1.2.6
  REGISTRY: https://<org>.jfrog.io/artifactoy
  REPOSITORY: your-artifactory-repo

jobs:
  verify:
    runs-on: ubuntu-latest

    steps:
      - name: Checkout code
        uses: actions/checkout@v2

      - name: Install Rust
        uses: actions-rs/toolchain@v1
        with:
          profile: minimal
          toolchain: nightly

      - name: Set up Rust environment
        run: |
          rustup target add aarch64-unknown-linux-gnu
        shell: bash

      - name: Install Buffrs
        run: |
          curl -L -o buffrs.tar.gz https://github.com/globusmedical/ext-buffrs/releases/download/gm%2Fv${BUFFRS_VERSION}/v${BUFFRS_VERSION}-x86_64-unknown-linux-gnu.tar.gz
          tar -xzf buffrs.tar.gz
          install -m 755 buffrs "$HOME/.local/bin/buffrs"
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
        shell: bash

      - name: Verify
        run: |
          echo $TOKEN | buffrs login --registry $REGISTRY
          buffrs lint
        env:
          TOKEN: ${{ secrets.BUFFRS_TOKEN }}
        shell: bash

  publish:
    runs-on: ubuntu-latest
    needs: build
    if: startsWith(github.ref, 'refs/tags/')

    steps:
      - name: Checkout code
        uses: actions/checkout@v2

      - name: Install Buffrs
        run: |
          curl -L -o buffrs.tar.gz https://github.com/globusmedical/ext-buffrs/releases/download/gm%2Fv${BUFFRS_VERSION}/v${BUFFRS_VERSION}-x86_64-unknown-linux-gnu.tar.gz
          tar -xzf buffrs.tar.gz
          install -m 755 buffrs "$HOME/.local/bin/buffrs"
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
        shell: bash

      - name: Publish on tag
        run: |
          echo $TOKEN | buffrs login --registry $REGISTRY
          buffrs publish --registry $REGISTRY --repository $REPOSITORY
        env:
          TOKEN: ${{ secrets.BUFFRS_TOKEN }}
        shell: bash
```

## GitLab CI

```yaml
stages:
  - verify
  - publish

variables:
  BUFFRS_VERSION: 1.2.6
  TOKEN: $BUFFRS_TOKEN # Your secret artifactory token
  REGISTRY: https://<org>.jfrog.io/artifactory
  REPOSITORY: your-artifactory-repo

verify:
  stage: verify
  script:
    - curl -L -o buffrs.tar.gz https://github.com/globusmedical/ext-buffrs/releases/download/gm%2Fv${BUFFRS_VERSION}/v${BUFFRS_VERSION}-x86_64-unknown-linux-gnu.tar.gz
    - tar -xzf buffrs.tar.gz
    - install -m 755 buffrs "$HOME/.local/bin/buffrs"
    - export PATH="$HOME/.local/bin:$PATH"
    - echo $TOKEN | buffrs login --registry $REGISTRY
    - buffrs lint
  only:
    - branches

publish:
  stage: publish
  script:
    - curl -L -o buffrs.tar.gz https://github.com/globusmedical/ext-buffrs/releases/download/gm%2Fv${BUFFRS_VERSION}/v${BUFFRS_VERSION}-x86_64-unknown-linux-gnu.tar.gz
    - tar -xzf buffrs.tar.gz
    - install -m 755 buffrs "$HOME/.local/bin/buffrs"
    - export PATH="$HOME/.local/bin:$PATH"
    - echo $TOKEN | buffrs login --registry $REGISTRY
    - buffrs publish --registry $REGISTRY --repository $REPOSITORY
  only:
    - tags
```
