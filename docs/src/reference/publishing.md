# Publishing on buff.rs

Publishing a package makes your protobuf definitions available to others via
a registry.

## Prerequisites

1. A `Proto.toml` manifest with a `[package]` section
2. Authentication with your registry (`buffrs login`)
3. A clean git working tree (or use `--allow-dirty`)

## Publishing a Package

```bash
buffrs publish --registry https://artifactory.company.com/artifactory --repository buffrs-local
```

### Options

| Option              | Description                              |
| ------------------- | ---------------------------------------- |
| `--registry`        | Artifactory URL                          |
| `--repository`      | Destination repository name              |
| `--allow-dirty`     | Allow publishing with uncommitted changes |
| `--dry-run`         | Package without uploading                |
| `--set-version`     | Override manifest version                |

## Package Preparation

Before publishing, buffrs:

1. Validates your `Proto.toml` manifest
2. Checks git working tree is clean
3. Packages all `.proto` files into a tarball
4. Uploads to the specified registry/repository

## Version Override

To publish with a different version than declared in `Proto.toml`:

```bash
buffrs publish --repository buffrs-local --set-version 2.0.0
```

## Dry Run

To test packaging without uploading:

```bash
buffrs publish --repository buffrs-local --dry-run
```

## See Also

- [Manifest Reference](manifest.md)
- [Registry Configuration](config.md)
