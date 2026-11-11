# bash-screen

Let an agent operate GUI apps on a remote Linux host over SSH, with a live
preview in a [herdr](https://github.com/herdrdev/herdr) pane.

Status: an early working slice. It has been exercised against one Ubuntu 24.04
host with Firefox and a few GTK apps.

## How it works

- The agent calls the `bash-screen` CLI. Each call is one SSH round trip to a
  helper on the remote host; one SSH connection is reused across calls.
- The remote host runs an isolated GUI session: an Xvnc virtual display, the
  openbox window manager, and a private session bus. Nothing listens on a
  network port.
- The helper reads the UI through
  [agent-desktop](https://github.com/vercel-labs/agent-desktop) (the
  accessibility tree) and falls back to screenshots and coordinates for
  anything the tree does not cover.
- Inside herdr, the first call opens a preview pane next to the calling pane.
  It shows the remote screen and the latest actions.

## Setup

### Install

```sh
herdr plugin install TolemyW/bash-screen
herdr plugin action invoke bash-screen.link-cli
```

The install step builds the binary and needs a Rust toolchain. The second
command links `bash-screen` into `~/.local/bin`; herdr repeats it at every
server start, so the link follows plugin updates. It never replaces a file
that is not its own link.

To work on the plugin from a checkout instead:

```sh
cargo build --release && mkdir -p bin && cp target/release/bash-screen bin/
herdr plugin link "$PWD"
herdr plugin action invoke bash-screen.link-cli
```

### Connect a remote host

```sh
bash-screen config --host <ssh-host>
bash-screen setup
```

`setup` needs no root. Into the remote directory it unpacks Xvnc and openbox
from the distribution's packages, and installs agent-desktop from npm. It can
be run again at any time; finished steps are skipped.

The remote host must be Ubuntu 24.04 or later and already have `dbus`,
`at-spi2-core`, `xauth`, `x11-xkb-utils`, `npm`, and Python 3 with Pillow.
`setup` checks for these and prints the `apt-get install` command an
administrator should run if any are missing.

## Use

```sh
bash-screen launch -- firefox https://example.org
bash-screen observe            # elements of the current window, each with a ref
bash-screen click e12          # a ref, or a screen position such as 640,400
bash-screen type --ref e7 hello
bash-screen key ctrl+l
bash-screen scroll --ref e40
bash-screen look               # screenshot saved locally as PNG
bash-screen vnc                # how a person can take over with a VNC client
bash-screen stop
```

Every action prints a fresh observation. Refs are valid only for the latest
observation. Coordinates are screen pixels.

## Preview

The preview draws a real image when the terminal supports the Kitty graphics
protocol, and block characters otherwise. Its text goes wherever the pane has
spare room: a column beside the picture, or lines below it.

Change the resolution of the picture at any time:

- in the preview pane, press `+` or `-`, or `1` to `4` for a specific step;
- from anywhere, run `bash-screen res up`, `res down`, `res 960` or `res full`;
- or bind the plugin actions `bash-screen.sharper` and `bash-screen.coarser`
  to keys in herdr.

The steps are 320, 640 and 960 pixels on the longest edge, and the full screen.

### Taking control

Press `i` in the preview pane to operate the remote screen yourself. The
heading turns red, and from then on clicks, the wheel, typed and pasted text
and key combinations all go to the remote screen. Press `Ctrl+]` to hand the
keys back to the preview.

- Each input takes one SSH round trip, so expect a fraction of a second
  between a click and the picture changing.
- The picture is sent at 960 pixels or more while you are in control.
- Text is entered by pasting, which replaces the remote clipboard.
- Dragging is not supported, and keys that herdr or the terminal keep for
  themselves never reach the pane.
- Your actions appear in the action log marked `you:`. Nothing stops you and
  the agent from acting at the same time.

For anything finer, `bash-screen vnc` prints how to attach a VNC client.

## Configuration

`bash-screen config` prints the file in use, normally
`~/.config/bash-screen/config.toml`.

| Key | Default | Meaning |
|---|---|---|
| `host` | | SSH host, as you would pass it to `ssh` |
| `remote_dir` | `~/.bash-screen` | Directory on the remote host holding the tools |
| `geometry` | `1280x800` | Size of the virtual screen, fixed per session |
| `preview_direction` | `auto` | Where the preview pane opens: `auto` (right of a wide pane, above a narrow one), `right`, `down` or `up` |
| `preview_blocks` | `auto` | How the preview draws: `auto`, `image`, `sextant`, `quad` or `half` |
| `preview_max_px` | `640` | Longest edge of the picture sent to the preview; `0` sends the full screen |

Environment variables:

| Variable | Effect |
|---|---|
| `BASH_SCREEN_CONFIG` | Use this config file |
| `BASH_SCREEN_NO_PREVIEW` | Do not open the preview pane |
| `BASH_SCREEN_DEBUG` | Make the preview log the timing of each redraw to `~/.local/state/bash-screen/preview-timing.log` |

## Known limits

- One session per remote host.
- No request de-duplication or resume after a dropped connection.
- The session shares the remote user's home directory; only the display and
  the session bus are isolated.
- The remote helper is a Python script standing in for a planned supervisor
  process.

## Layout

| Path | Contents |
|---|---|
| `src/` | The local CLI and preview (Rust) |
| `remote/` | Files copied to the remote host: installer, session launcher and helper |
| `herdr-plugin.toml` | herdr plugin manifest |
| `scripts/` | Helper run by the plugin to put the command on `PATH` |
| `tests/fixture/` | Test page and request-logging server used as a test oracle |

## License

Apache-2.0. See `LICENSE`.
