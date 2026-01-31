# Environment Variables

Buffrs behavior can be customized via environment variables. These variables take
precedence over configuration file settings.

## Index

- [`BUFFRS_HOME`](#buffrs_home) — Location of buffrs home directory
- [`BUFFRS_CACHE`](#buffrs_cache) — Location of the package cache
- [`BUFFRS_TESTSUITE`](#buffrs_testsuite) — Skip git dirty checks in tests

## Reference

### `BUFFRS_HOME`

The home directory for buffrs where credentials and global configuration are stored.

**Default:** `~/.buffrs`

If not set, buffrs defaults to the `.buffrs` folder in the user's home directory.

```powershell
$env:BUFFRS_HOME = "D:\buffrs"
```

```bash
export BUFFRS_HOME="/opt/buffrs"
```

### `BUFFRS_CACHE`

Override the location of the package cache directory where downloaded packages are
stored.

**Default:** System temp directory

```powershell
$env:BUFFRS_CACHE = "D:\buffrs-cache"
```

```bash
export BUFFRS_CACHE="/var/cache/buffrs"
```

### `BUFFRS_TESTSUITE`

When set to any value, disables interactive prompts and git dirty checks during
`buffrs publish`. This is primarily used by the test suite to run non-interactively.

**Default:** Not set

```powershell
$env:BUFFRS_TESTSUITE = "1"
```

```bash
export BUFFRS_TESTSUITE=1
```
