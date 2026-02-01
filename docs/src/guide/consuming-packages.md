## Consuming Packages

As described in the [package types](./package-types.md) section you can declare
dependencies through your project's `Proto.toml`. This is true for both
libraries and APIs. But what if you want to implement your server and you need
to consume buffrs packages that contain your type and service definitions?

So there are three scenarios in which you would want to depend on other packages:

a) You are defining a library and you want to make use of an external type
   coming from another library (e.g. `google` for basic types such as
   `google.None`).
b) You are defining an API and you want to use libraries to reuse common types
   for that domain (e.g. `time` or `physics`)
c) You are implementing your server and you want to get access to API
   definitions to generate bindings.

The good news are: They are all achieved in a similar fashion. You make use of
the `[dependencies]` key in your manifest to declare the packages that your
projects needs – either to publish another package or to compile to protos to
bindings.

### Examples

#### Libraries & APIs

> This section is identical for libraries and APIs.

An example of the `time` library reusing the `google` library:

```
[package]
name = "time"
type = "lib"
version = "1.0.0"

[dependencies]
google = { version = "=1.0.0", registry = "<your-registry>", repository = "<your-repository>" }
```

Running `buffrs install` yields you with the following filesystem:


```text
time
├── Proto.toml
└── proto
    ├── time.proto
    └── vendor
        ├── time
        ├   └── ..
        └── google
            ├── any.proto
            ├── ..
            ├── struct.proto
            └── timestamp.proto
```

You can now develop your library and publish it using `buffrs publish`.

##### Servers

If you want to implement your server and thus use e.g. a `logging` API the only
major difference is the lack of the `[package]` section in your manifest.

```
[dependencies]
logging = { version = "=1.0.0", registry = "<your-registry>", repository = "<your-repository>" }
```

##### Multi-Version Dependencies

In larger projects, you may need different parts of your codebase to use
different versions of the same package. Starting with buffrs 0.50.0, you can
enable this with `resolver = "multiversion"`:

```toml
[dependencies]
logging-v1 = { package = "logging", version = "=1.0.0", resolver = "multiversion", registry = "<your-registry>", repository = "<your-repository>" }
logging-v2 = { package = "logging", version = "=2.0.0", resolver = "multiversion", registry = "<your-registry>", repository = "<your-repository>" }
```

See [Multi-Version Dependencies](./multi-version-dependencies.md) for complete documentation.

Running a `buffrs install` yields you the very same as above, except for the
omitted local package and the `logging` dependency instead of `time`.

```text
.
├── Proto.toml
└── proto
    └── vendor
        └── logging
            └── logger.proto
```
