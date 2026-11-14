---
name: bash-screen
description: Operate GUI apps (a browser, desktop apps) on a remote Linux host over SSH with the bash-screen CLI - launch an app, list the elements of its window, click, type, press keys, scroll and take screenshots. Use when a task needs a graphical app on a remote machine or on this Linux machine, such as opening a web page that is only reachable from that host, clicking through a web UI, or checking how something looks. Trigger keywords - bash-screen, remote desktop, remote browser, GUI, click, screenshot, firefox on the server.
license: Apache-2.0
---

# bash-screen

`bash-screen` drives GUI apps on a Linux host from the command line. Each
command is one SSH round trip. The apps run on a virtual screen on the host,
separate from any desktop a person is using there.

## Before you start

```sh
bash-screen config      # the default host; an error means none is set up yet
bash-screen status      # whether its screen is running, and the open windows
```

If no host is set up, ask the user which SSH host to use, then run
`bash-screen init <ssh-host>`. It installs what the host needs without root
and can be run again safely. If it reports missing packages, pass its
`apt-get install` line to the user; do not try to get root yourself.

If the command itself is missing, see
https://github.com/TolemyW/bash-screen#quick-start.

## The loop

```sh
bash-screen launch -- firefox https://example.org
bash-screen observe            # elements of the current window, each with a ref like e12
bash-screen click e12          # or a screen position: bash-screen click 640,400
bash-screen type --ref e7 hello
bash-screen key ctrl+l         # several in order: bash-screen key ctrl+l return
bash-screen scroll --dy -5     # negative is down; --ref e40 scrolls an element into view
bash-screen look               # screenshot saved locally as PNG; prints the path
```

Rules that matter:

- Every action prints a fresh observation. Read it instead of calling
  `observe` again.
- A ref is valid only for the latest observation. After any action, use the
  refs from the new output, never old ones.
- Prefer refs. Use positions when the element you need has no ref.
- Positions are screen pixels, the same as in a `look` image at scale 1.
- `observe` lists interactive elements. Add `--all` for headings and text.
- When the list does not tell you enough, run `look` and read the PNG.
  `--region x,y,width,height` limits it to part of the screen.
- `type` without `--ref` pastes at the keyboard focus. Pasting replaces the
  clipboard of the remote screen.
- `launch --wait <seconds>` gives a slow app longer before the first
  observation (default 4).

## More commands

```sh
bash-screen click e12 --count 2          # double click; --button right for a context menu
bash-screen drag 200,300 600,300
bash-screen windows                      # the windows on the screen
bash-screen --json observe               # any reply as one line of JSON
bash-screen stop                         # end the screen and its apps
```

## Several hosts or screens

Commands act on the default host and its session `main`.

```sh
bash-screen --host other-host observe
bash-screen --session research launch -- firefox
bash-screen sessions
```

- A session is a separate screen with its own apps and browser profile. If
  another agent or person may be using `main`, work in a session of your own:
  set `BASH_SCREEN_SESSION=<name>` for your shell and stop it when done.
- `BASH_SCREEN_HOST` does the same for the host.
- The host `local` is this machine, without SSH (Linux only).
- `stop` keeps the browser profile, so logins survive. `stop --forget`
  deletes it.

## When something goes wrong

- An app crashes or hangs after `observe` or after a key press (seen with
  Electron apps): start it with `bash-screen launch --no-a11y -- <app>`.
  `observe` then cannot see it; work from `look` and positions.
- A dialog covers the page: `observe` still lists the covered elements,
  before the dialog's own. Check with `look` which ones are on top.
- A dropped connection is not resumed. Run the command again after checking
  with `observe` whether the action already took effect.

## The person watching

Inside herdr or the Herdr desktop app, the first command opens a live preview
pane beside your terminal; that is expected. Set `BASH_SCREEN_NO_PREVIEW=1`
to prevent it. The person can take control of the screen from that pane, and
their actions appear in the action log marked `you:`. `bash-screen vnc`
prints how they can attach a VNC client instead.
