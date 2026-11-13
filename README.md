# bash-screen

Let an agent operate GUI apps on a remote Linux host over SSH, with a live
preview in the terminal. It is a plain command-line tool, and also a
[herdr](https://github.com/herdrdev/herdr) plugin that opens the preview by
itself.

Status: an early working slice. The remote host must be Ubuntu 22.04 or
later, or Debian 12 or later.

## Quick start

Install:

```sh
herdr plugin install TolemyW/bash-screen
```

Without herdr, download the binary for your system from the
[releases](https://github.com/TolemyW/bash-screen/releases) and put it on your
`PATH` as `bash-screen`.

Connect a host you can already reach with `ssh`:

```sh
bash-screen init <ssh-host>
```

This installs the remote tools, starts a screen and reads it back once as a
check. It needs no root and can be run again at any time. If the host lacks a
package, it prints the `apt-get install` command an administrator should run.

## Commands

```sh
bash-screen launch -- firefox https://example.org
bash-screen observe            # elements of the current window, each with a ref
bash-screen click e12          # a ref, or a screen position such as 640,400
bash-screen type --ref e7 hello
bash-screen key ctrl+l
bash-screen scroll --ref e40
bash-screen drag 200,300 600,300
bash-screen look               # screenshot saved locally as PNG
bash-screen stop
```

- Every action prints a fresh observation. Refs are valid only for the latest
  observation. Coordinates are screen pixels.
- `--json` on any command gives the reply as one line of JSON. For `look` the
  JSON names the saved file.
- If an app misbehaves when its elements are read, start it with
  `bash-screen launch --no-a11y -- <app>`. `observe` then does not see it;
  work from `look` and positions instead.

## Preview

Inside herdr or the Herdr desktop app, the first command opens a preview pane
next to the pane that ran it. Anywhere else, run `bash-screen preview` in a
terminal of your own.

In the preview pane:

| Key | Effect |
|---|---|
| `+` `-` | Sharper or coarser picture |
| `1` to `4` | A specific resolution: 320, 640 or 960 pixels, or the full screen |
| `i` | Take control: clicks, the wheel, typed and pasted text and key combinations go to the remote screen |
| `Ctrl+]` | Hand the keys back to the preview |

While you are in control the heading is red. Each input takes one SSH round
trip, so expect a fraction of a second between a click and the picture
changing. For anything finer, `bash-screen vnc` prints how to attach a VNC
client.

## Several hosts or screens

Commands act on the default host, which is the first one you `init`, and its
session `main`. A session is a separate screen with its own apps, browser
profile and action log, so several agents can share one host without seeing
each other.

```sh
bash-screen init other-host                         # a second host; the default stays
bash-screen --host other-host observe
bash-screen --session research launch -- firefox    # a second screen on the default host
bash-screen sessions                                # what is running on the host
bash-screen --session research stop
```

- `BASH_SCREEN_HOST` and `BASH_SCREEN_SESSION` select the same things for a
  whole shell, which is the easy way to give each agent its own session.
- The host `local` is this machine, reached without SSH.
- Stopping a session keeps its browser profile; `stop --forget` deletes it.

## More

- [docs/reference.md](docs/reference.md): how it works, remote host requirements,
  configuration, environment variables and known limits.
- [CONTRIBUTING.md](CONTRIBUTING.md): building from a checkout and where
  things are in the repository.

## License

Apache-2.0. See `LICENSE`.
