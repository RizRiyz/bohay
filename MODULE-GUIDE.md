# Writing a bohay module

**This guide has moved to the documentation site:**

→ **[Writing a Module](https://bohay.dev/docs/extend/writing-modules/)** —
the complete guide: manifest reference, environment variables, the context
blob, calling back into bohay, distribution, and troubleshooting.

Quick taste — a module is a directory with a `bohay-module.toml` manifest
declaring argv commands, in any language, no SDK:

```toml
id = "you.hello"
name = "Hello"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[actions]]
id = "greet"
command = ["sh", "greet.sh"]
```

```sh
bohay module link .              # register it
bohay module run you.hello greet
```

See also [Using Modules](https://bohay.dev/docs/extend/using-modules/)
for discovering and installing community modules.
