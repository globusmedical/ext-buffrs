# Buffrs

Package management for versioned Protocol Buffer and gRPC APIs.

[![Repository](https://img.shields.io/badge/github-globusmedical%2Fext--buffrs-181717?logo=github)](https://github.com/globusmedical/ext-buffrs)
[![Buffrs Book](https://img.shields.io/badge/book-internal%20docs-blueviolet.svg)](https://globusmedical.github.io/ext-buffrs/)
[![Releases](https://img.shields.io/github/v/release/globusmedical/ext-buffrs?display_name=tag&sort=semver)](https://github.com/globusmedical/ext-buffrs/releases)

Buffrs helps teams version, publish, and consume shared protobuf definitions as reusable API packages. In the Globus Medical fork, the focus is reliable dependency management for internal gRPC APIs: full semantic version support, reproducible lockfiles, and side-by-side multi-version installs when migrations overlap.

Install Buffrs from [GitHub Releases](https://github.com/globusmedical/ext-buffrs/releases) or build it from source in this repository.

## Quickstart

```bash,ignore
buffrs login --registry https://your-registry.example.com/artifactory
buffrs init --api
buffrs add --registry https://your-registry.example.com/artifactory team/my-api@^1.2.3
buffrs install
```

Useful resources:

- [The Buffrs Book](https://globusmedical.github.io/ext-buffrs/)
- [GitHub Releases](https://github.com/globusmedical/ext-buffrs/releases)
- [Source Repository](https://github.com/globusmedical/ext-buffrs)
- `buffrs help`

## Synopsis

```text,ignore
Package management for versioned Protocol Buffer and gRPC APIs

Usage: buffrs <COMMAND>

Commands:
  init         Initializes a buffrs setup
  new          Creates a new buffrs package in the current directory
  lint         Check rule violations for this package
  add          Adds dependencies to a manifest file
  remove       Removes dependencies from a manifest file
  package      Exports the current package into a distributable tgz archive
  publish      Packages and uploads this api to the registry
  install      Installs dependencies
  uninstall    Uninstalls dependencies
  list         Lists all protobuf files managed by Buffrs to stdout
  login        Logs you in for a registry
  logout       Logs you out from a registry
  lock         Lockfile related commands
  self-update  Update buffrs to the latest version
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

## Why teams use Buffrs

- **Full SemVer support for gRPC API dependencies**: use `^`, `~`, ranges, exact pins, and prereleases in `Proto.toml`.
- **Multi-version API support for migrations**: keep two versions of the same API side by side when a rollout cannot happen all at once.
- **Package aliases for side-by-side versions**: declare the same package twice under different keys when multi-version dependencies are enabled.
- **Reproducible installs**: lock exact resolutions in `Proto.lock`.
- **Build-tool integration**: integrate generated artifacts into Cargo, CMake, Python, and other workflows.

## Documentation

Start with [the book](https://globusmedical.github.io/ext-buffrs/) for installation, workflows, and command reference.
