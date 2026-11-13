# bash-screen reference

Material to look up. For a first run, see the [README](../README.md).

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

## Install

`herdr plugin install` downloads the binary for your system from the GitHub
release, checks it against the release's checksums, and links `bash-screen`
into `~/.local/bin`. The link is refreshed at every herdr start, so it follows
plugin updates. It never replaces a file that is not its own link.

Where there is no prebuilt binary the install builds from source instead,
which needs Rust. Set `BASH_SCREEN_BUILD_FROM_SOURCE=1` to force that.

## Remote host

Tested on Ubuntu 22.04, 24.04 and 26.04 and Debian 12 and 13. Older releases
do not work, because agent-desktop needs glibc 2.35.

The host must already have these packages:

- `dbus`, `at-spi2-core`, `xauth`, `x11-xkb-utils`
- Python 3 with Pillow
- on Ubuntu 22.04 also `xkb-data`, which any host with a desktop has

`init` checks for them and prints the `apt-get install` command an
administrator should run if any are missing. Everything else (Xvnc, openbox,
agent-desktop) is unpacked into the remote directory without root.

`bash-screen init` runs three steps that are also available on their own:
`config`, `setup` and `start`. Finished steps are skipped when it runs again.
`--remote-dir` and `--geometry` change the defaults.

## Preview

The preview pane follows the session its calling pane last used. In the Herdr
desktop app it needs no plugin, only `bash-screen`, `herdr-app` and `hc` on
the `PATH`. `bash-screen preview --host <host> --session <name>` chooses what
a preview of your own shows.

It draws a real image when the terminal supports the Kitty graphics protocol,
and block characters otherwise. Its text goes wherever the pane has spare
room: a column beside the picture, or lines below it.

Besides the keys in the preview pane, the resolution can be changed:

- from anywhere, with `bash-screen res up`, `res down`, `res 960` or
  `res full`;
- in herdr, by binding the plugin actions `bash-screen.sharper` and
  `bash-screen.coarser` to keys.

While you are in control of the screen:

- The picture is sent at 960 pixels or more.
- Text is entered by pasting, which replaces the remote clipboard.
- Dragging with the mouse is not supported, and keys that herdr or the
  terminal keep for themselves never reach the pane.
- Your actions appear in the action log marked `you:`. Nothing stops you and
  the agent from acting at the same time.

## Configuration

`bash-screen config` prints the file in use, normally
`~/.config/bash-screen/config.toml`.

| Key | Default | Meaning |
|---|---|---|
| `host` | | The default host: an SSH host as you would pass it to `ssh`, or `local` |
| `remote_dir` | `~/.bash-screen` | Directory on the default host holding the tools |
| `geometry` | `1280x800` | Size of the virtual screen, fixed per session |
| `preview_direction` | `auto` | Where the preview pane opens: `auto` (right of a wide pane, above a narrow one), `right`, `down` or `up` |
| `preview_blocks` | `auto` | How the preview draws: `auto`, `image`, `sextant`, `quad` or `half` |
| `preview_max_px` | `640` | Longest edge of the picture sent to the preview; `0` sends the full screen |

Another host can have its own `remote_dir` and `geometry`:

```toml
[hosts.other-host]
remote_dir = "~/tools/bash-screen"
```

## Environment variables

| Variable | Effect |
|---|---|
| `BASH_SCREEN_CONFIG` | Use this config file |
| `BASH_SCREEN_HOST` | Host to act on, as `--host` |
| `BASH_SCREEN_SESSION` | Session to act on, as `--session` |
| `BASH_SCREEN_NO_PREVIEW` | Do not open the preview pane |
| `BASH_SCREEN_BUILD_FROM_SOURCE` | Build from source at install time instead of downloading a binary |
| `BASH_SCREEN_DEBUG` | Make the preview log the timing of each redraw to `~/.local/state/bash-screen/preview-timing.log` |

## Known limits

- Electron and Chromium apps can crash on a key press after their elements
  have been read. Start them with `launch --no-a11y`. Only one Electron app
  has been tried, and Chrome itself has not.
- When a dialog covers a page, `observe` still lists the covered elements,
  ahead of the dialog's own.
- A dropped connection is not resumed, and a repeated request is not
  recognised as a repeat.
- A session shares the remote user's home directory; only the display and the
  session bus are isolated. Each session does get its own Firefox profile.
