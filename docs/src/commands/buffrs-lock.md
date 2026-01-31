## buffrs lock

Lockfile-related commands.

## Subcommands

### `buffrs lock print-files`

Prints the file requirements derived from the lockfile serialized as JSON.

This command is useful for consumption of the lockfile data in other programs
and build systems.

```bash
buffrs lock print-files
```

**Example output:**

```json
{
  "packages": [
    {
      "name": "my-api",
      "version": "1.0.0",
      "registry": "https://artifactory.company.com/artifactory",
      "repository": "buffrs-local"
    }
  ]
}
```

## See Also

- [Lockfile Reference](../reference/lockfile.md)
