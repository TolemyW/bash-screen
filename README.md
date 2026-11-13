# bash-screen

Let an agent operate GUI apps on a remote Linux host over SSH, with a live
preview in the terminal. It is a plain command-line tool, and also a
[herdr](https://github.com/herdrdev/herdr) plugin that opens the preview by
itself.

Status: an early working slice. The remote side is tested on Ubuntu 22.04,
24.04 and 26.04 and Debian 12 and 13. Apps exercised so far: Firefox, a few
GTK apps, and one Electron app (see Known limits).

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
- `bash-screen preview` shows the screen and the latest actions in any
  terminal. Inside herdr or the Herdr desktop app, the first call opens it
  in a pane next to the calling pane.

## Setup

### Install

```sh
herdr plugin install TolemyW/bash-screen
```

This downloads the binary for your system from the GitHub release, checks it
against the release's checksums, and links `bash-screen` into `~/.local/bin`.
No toolchain is needed. Where there is no prebuilt binary it builds from
source instead, which needs Rust; set `BASH_SCREEN_BUILD_FROM_SOURCE=1` to
force that.

The link is refreshed at every herdr start, so it follows plugin updates. It
never replaces a file that is not its own link.

Without herdr, download the binary for your system from the
[releases](https://github.com/TolemyW/bash-screen/releases) and put it on your
`PATH` as `bash-screen`.

To work on the plugin from a checkout:

```sh
BASH_SCREEN_BUILD_FROM_SOURCE=1 sh scripts/install.sh
herdr plugin link "$PWD"
```

### Connect a remote host

```sh
bash-screen init <ssh-host>
```

This writes the config, installs the remote tools, starts a session and reads
the screen back once as a check. It needs no root and can be run again at any
time; finished steps are skipped. `--remote-dir` and `--geometry` change the
defaults. The single steps are also available as `config`, `setup` and
`start`.

On the remote host it unpacks Xvnc and openbox from the distribution's
packages and downloads agent-desktop, all inside the remote directory.

The remote host must be Ubuntu 22.04 or later, or Debian 12 or later
(agent-desktop needs glibc 2.35), and already have `dbus`, `at-spi2-core`,
`xauth`, `x11-xkb-utils`, and Python 3 with Pillow (on Ubuntu 22.04 also
`xkb-data`, which any host with a desktop has). `init` checks for these
and prints the `apt-get install` command an administrator should run if any
are missing.

### Hosts and sessions

Commands act on the default host and its session `main`. The first host you
`init` becomes the default.

```sh
bash-screen init other-host                 # a second host; the default stays
bash-screen --host other-host observe
bash-screen --session research launch -- firefox   # a second screen on the default host
bash-screen sessions                        # what is running on the host
bash-screen --session research stop
```

- A session is a separate screen with its own apps, session bus, Firefox
  profile and action log. Sessions on one host share only the installed
  tools, so several agents can work on one host without seeing each other.
- `BASH_SCREEN_HOST` and `BASH_SCREEN_SESSION` select the same things for a
  whole shell, which is the easy way to give each agent its own session.
- The host `local` is this machine, reached without SSH: for an agent that
  runs on the same Linux machine as the apps.
- Stopping a session keeps its browser profile; starting it again continues
  from there. `stop --forget` deletes the profile as well.

## Use

```sh
bash-screen launch -- firefox https://example.org
bash-screen observe            # elements of the current window, each with a ref
bash-screen click e12          # a ref, or a screen position such as 640,400
bash-screen type --ref e7 hello
bash-screen key ctrl+l
bash-screen scroll --ref e40
bash-screen drag 200,300 600,300
bash-screen look               # screenshot saved locally as PNG
bash-screen vnc                # how a person can take over with a VNC client
bash-screen stop
```

Every action prints a fresh observation. Refs are valid only for the latest
observation. Coordinates are screen pixels.

Add `--json` to any command to get its reply as one line of JSON instead of
text. For `look` the JSON names the saved file.

If an app misbehaves when its elements are read, start it with
`bash-screen launch --no-a11y -- <app>`. `observe` then does not see it; work
from `look` and positions instead.

## Preview

Inside herdr the preview pane opens by itself and follows the session its
calling pane last used. The same happens in the Herdr desktop app, which needs
no plugin: with `bash-screen`, `herdr-app` and `hc` on the `PATH`, the first
command run in one of its terminals splits that pane. Anywhere else, run
`bash-screen preview` in a terminal of your own; `--host` and `--session`
choose what it shows.

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
- Dragging with the mouse is not supported, and keys that herdr or the
  terminal keep for themselves never reach the pane.
- Your actions appear in the action log marked `you:`. Nothing stops you and
  the agent from acting at the same time.

For anything finer, `bash-screen vnc` prints how to attach a VNC client.

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

Environment variables:

| Variable | Effect |
|---|---|
| `BASH_SCREEN_CONFIG` | Use this config file |
| `BASH_SCREEN_HOST` | Host to act on, as `--host` |
| `BASH_SCREEN_SESSION` | Session to act on, as `--session` |
| `BASH_SCREEN_NO_PREVIEW` | Do not open the preview pane |
| `BASH_SCREEN_DEBUG` | Make the preview log the timing of each redraw to `~/.local/state/bash-screen/preview-timing.log` |

## Known limits

- Electron and Chromium apps: with Clash Party (Electron 43) the app crashed
  on the first key press after its elements had been read. Started with
  `--no-a11y` it took keys without trouble. Chrome itself has not been tried.
- When a dialog covers a page, `observe` still lists the covered elements,
  ahead of the dialog's own.
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
| `scripts/` | Install step of the plugin, and the helper that puts the command on `PATH` |
| `.github/workflows/` | Tests on every push; binaries published for every version tag |
| `tests/remote-setup.sh` | End-to-end check of the remote side on a bare machine |
| `tests/local-e2e.sh` | End-to-end check of the CLI with this machine as the host |
| `tests/fixture/` | Test page and request-logging server used as a test oracle |

## License

Apache-2.0. See `LICENSE`.
