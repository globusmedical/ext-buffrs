# FAQ

## Why doesn't `buffrs add`, `buffrs publish`, or `buffrs login` work anymore?

> [!IMPORTANT]
> Recent versions of Buffrs support multiple registries. Commands now require the `--registry` flag.

We expanded Buffrs to handle connections to multiple registries simultaneously. Add `--registry https://your-registry.com/artifactory` to these commands:

```bash
buffrs add --registry https://your-registry.com/artifactory <package>
buffrs publish --registry https://your-registry.com/artifactory
buffrs login --registry https://your-registry.com/artifactory
```

> [!NOTE]
> The `buffrs login` flag was renamed from `--url` to `--registry` for consistency.

## Why is my `credentials.toml` file broken?

> [!IMPORTANT]
> The credentials file format changed to support multiple registries.

**Old format** (single registry):

```toml
[artifactory]
url = "https://org.jfrog.io/artifactory"
password = "some-token"
```

**New format** (multiple registries):

```toml
[[credentials]]
uri = "https://org1.jfrog.io/artifactory"
token = "some-token"

[[credentials]]
uri = "https://org2.jfrog.io/artifactory"
token = "some-other-token"
```

> [!TIP]
> Run `buffrs login --registry <url>` to automatically update your credentials file.

## Why can't I log in with a username?

> [!IMPORTANT]
> Username-based authentication is no longer supported. Use tokens instead.

The `--username` flag has been removed in favor of token-based authentication. This enables support for:

- Identity tokens
- JWT tokens
- Encoded basic auth tokens

All authentication now uses the `Authorization` header for better security and flexibility.

> [!TIP]
> Generate an access token from your registry provider and use `buffrs login --registry <url>` to store it.
