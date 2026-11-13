# Environment Variables

Buffrs behavior can be customized via environment variables. These variables take
precedence over configuration file settings.

## Index

- [`BUFFRS_HOME`](#buffrs_home) — Location of buffrs home directory
- [`BUFFRS_CACHE`](#buffrs_cache) — Location of the package cache
- [`BUFFRS_HTTP_CONNECT_TIMEOUT`](#buffrs_http_connect_timeout) — Time allowed for connecting to a registry
- [`BUFFRS_HTTP_TIMEOUT`](#buffrs_http_timeout) — Time a registry may stay silent during a request
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

### `BUFFRS_HTTP_CONNECT_TIMEOUT`

Time in seconds that buffrs waits for a connection to a registry. `0` removes the limit.

**Default:** `30`

```powershell
$env:BUFFRS_HTTP_CONNECT_TIMEOUT = "60"
```

```bash
export BUFFRS_HTTP_CONNECT_TIMEOUT=60
```

### `BUFFRS_HTTP_TIMEOUT`

Time in seconds that a registry may stay silent during a request: until the response
headers arrive, and between two reads of the response body. A download of any size
continues for as long as data arrives. An upload by `buffrs publish` has to be sent and
answered within this time, so raise the value on a very slow link. `0` removes the limit.

**Default:** `60`

A request that fails to connect, times out or loses its connection is sent again, as is
one that the registry answers with `429` or `503`, and a download or version listing whose
response is cut off or stalls part way through the body: up to six times in total and for at
most two minutes, waiting between attempts for the time the registry asks for in
`Retry-After` or else for one second, doubling each time. An upload is only sent again
if no connection could be established.

```powershell
$env:BUFFRS_HTTP_TIMEOUT = "120"
```

```bash
export BUFFRS_HTTP_TIMEOUT=120
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
