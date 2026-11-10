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

### Remote host

Ubuntu 24.04 or later. The remote directory must already contain the tools:

- `root/usr/bin/Xtigervnc` and `root/usr/bin/openbox`, with their libraries
  under `root/usr/lib/x86_64-linux-gnu`, unpacked from the distribution
  packages (`apt-get download` and `dpkg -x` work without root);
- `ad/node_modules/.bin/agent-desktop`, version 1.2.0
  (`npm install --prefix ad agent-desktop@1.2.0`);
- Python 3 with Pillow.

Installing these automatically is not implemented yet.

### Local machine

```sh
cargo build --release && mkdir -p bin && cp target/release/bash-screen bin/
herdr plugin link "$PWD"
bin/bash-screen config --host <ssh-host> --remote-dir <remote-directory>
```

Put `bin/bash-screen` on the agent's `PATH`.

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
| `remote/` | Files copied to the remote host: session launcher and helper |
| `herdr-plugin.toml` | herdr plugin manifest |
| `tests/fixture/` | Test page and request-logging server used as a test oracle |
