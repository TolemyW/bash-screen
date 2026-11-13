# Working on bash-screen

## Install from a checkout

```sh
BASH_SCREEN_BUILD_FROM_SOURCE=1 sh scripts/install.sh
herdr plugin link "$PWD"
```

This needs Rust.

## Layout

| Path | Contents |
|---|---|
| `src/` | The local CLI and preview (Rust) |
| `remote/` | Files copied to the remote host: installer, session launcher and helper |
| `herdr-plugin.toml` | herdr plugin manifest |
| `scripts/` | Install step of the plugin, and the helper that puts the command on `PATH` |
| `.github/workflows/` | Tests on every push; binaries published for every version tag |
| `tests/remote-setup.sh` | End-to-end check of the remote side on a bare machine |
| `tests/local-e2e.sh` | End-to-end check of the CLI with this machine as the host |
| `tests/fixture/` | Test page and request-logging server used as a test oracle |

## Tests

```sh
cargo test --release
tests/local-e2e.sh target/release/bash-screen
```

The second one needs a Debian or Ubuntu machine with the packages listed in
[docs/reference.md](docs/reference.md#remote-host).
