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
| `skills/bash-screen/SKILL.md` | The Agent Skill; built into the binary and printed by `bash-screen skill` |
| `helper/` | The helper that runs on the remote host (Rust), released as a static program for Linux |
| `remote/` | Scripts copied to the remote host: installer and session launcher, and `bs_remote.py`, the earlier helper |
| `herdr-plugin.toml` | herdr plugin manifest |
| `herdr-app/` | The mod that shows the remote screen in a pane of the Herdr app |
| `docs/reference.md` | Reference: configuration, host requirements, command details |
| `scripts/` | Install step of the plugin, and the helper that puts the command on `PATH` |
| `.github/workflows/` | Tests on every push; binaries published for every version tag |
| `tests/remote-setup.sh` | End-to-end check of the remote side on a bare machine |
| `tests/local-e2e.sh` | End-to-end check of the CLI with this machine as the host |

## Tests

```sh
cargo test --release --workspace
python3 -m unittest discover -s tests -p 'test_web.py'
node --check web/viewer.js
cargo build --release --workspace
BASH_SCREEN_HELPER_FILE="$PWD/target/release/bs-helper" tests/local-e2e.sh target/release/bash-screen
```

Use synthetic form values and example hostnames in fixtures and documentation.
Keep local configuration, session logs, viewer URLs with tokens, and screenshots
of personal sessions out of contributions.

The HTTP tests use only Python's standard library and loopback sockets. The
JavaScript syntax check needs Node.js. The last two commands need a Debian or
Ubuntu machine with the packages listed in
[docs/reference.md](docs/reference.md#remote-host). Until a version is
released, its helper is not published, so a checkout always passes its own
with `BASH_SCREEN_HELPER_FILE`.
