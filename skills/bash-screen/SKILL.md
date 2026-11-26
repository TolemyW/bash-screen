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

## Make few calls

A command takes a fraction of a second on the host, but each tool call you
make to run one costs several seconds more. The number of your tool calls,
not of commands, decides how long a task takes. So:

- Put the steps you already know into one `do`, and end it with `then text`
  when you need to read the result.
- Do not check each step with a separate call. Every action prints what it
  changed, such as `~ [e26] textfield "Email" = "ada@example.org"`; that is
  the check.
- One shell call can run several commands: `bash-screen observe && bash-screen text`.
- A form takes about three calls: one to see its elements, one `do` that
  fills it and ends with `text` to check the values, and one that submits,
  waits for the result and reads it.

## The loop

```sh
bash-screen launch -- firefox https://example.org   # prints the elements, each with a ref like e12
bash-screen do type --ref e7 "Ada" then type --ref e8 "ada@example.org" then click --name Role then click --name Admin then text
# compare the values in the text with the ones asked for, then:
bash-screen do click --name Submit then wait --text Saved then text
```

Single commands, for when one step is all you know:

```sh
bash-screen observe            # elements of the current window, each with a ref like e12
bash-screen click e12          # or a screen position: bash-screen click 640,400
bash-screen click --name Save  # or the one element with that name
bash-screen type --ref e7 hello          # or: bash-screen type --name Email hello
bash-screen key ctrl+l         # several in order: bash-screen key ctrl+l return
bash-screen scroll --dy -5     # negative is down; --ref e40 scrolls an element into view
bash-screen text               # the text of the page, to read it
```

## Batches

```sh
bash-screen do click e7 then type "Ada" then key tab then type "ada@example.org" then key return then wait --text Saved then text
```

- Steps are `click`, `type`, `key`, `scroll`, `drag` and `wait`, written as
  usual and separated by the bare word `then`. The last step can also be
  `text`, or `look` with its usual options.
- Refs in every step are the ones from the observation you last saw, even
  though earlier steps change the window.
- A step with `--name` looks for the element when its turn comes, so it can
  aim at something an earlier step brings up, such as the option of a
  drop-down list: `bash-screen do click --name Project then click --name "my-project"`.
- If a step fails, the later ones are not run. The error names the step and
  is followed by what the steps before it changed, so you need no `observe`.
  If only the last `text` or `look` fails, the error says all steps were
  done: do not run them again.
- End the batch where you need to read something before deciding.

## When an element is not found

Change the method instead of trying the same one again:

1. `--name` matches several elements: the error lists them; use the ref.
2. `--name` matches none: find the element in the list you have, or in
   `observe --all`, and use its ref or its exact name.
3. It is not in any list (some custom drop-downs and canvases): take
   `look --region` around where it should be, and click the position.

Do not try more than one other name before moving to the next way.

## Reading pages and filling forms

Use the remote accessibility tree for routine browser work, including when a
person has the HTML viewer open. The viewer displays a remote screen image.

- Read page content with `text`. It shows the content of inputs in `[ ]` and
  inline validation messages, so it is also how you check a form before
  submitting it. Use `observe --all` when you need the labels around a field
  to tell which one it is.
- Check the values once, before submitting, and the confirmation or error
  once, after it. Steps in between need no extra reading.
- Use `look` for a requested visual check or a specific question these
  commands cannot answer, such as which control an overlay covers. Limit
  it to the relevant region when its bounds are known. Resume using refs
  and text once the question is resolved.

## Wait instead of guessing

After anything that loads or takes time, wait for a sign of the result rather
than observing again and again:

```sh
bash-screen wait --text "Order placed"    # some element's name or value contains it (case-insensitive)
bash-screen wait --gone "Loading"         # no element contains it any more
bash-screen wait --title Dashboard        # the window title contains it
bash-screen wait                          # the screen has stopped changing
```

`wait` prints an observation when the condition holds, and fails after
`--timeout` seconds (default 15) saying what was still missing. Several
conditions can be combined; all must hold. With `launch --no-a11y` apps only
the plain `wait` works.

## Rules that matter

- Every action prints what it changed in the window: `+` an element that is
  new, `~` one that reads differently now, `-` one that has gone. Everything
  else is as in the list you already have. When much changed, the whole list
  is printed instead. Read this instead of calling `observe` again.
- A ref stays with its element for as long as the element is in the window,
  so refs from earlier output remain good. A ref whose element has gone
  fails; it never lands on another element.
- Elements that read the same, such as a `Delete` button in every row, are
  told apart by their order. When one of them goes, the refs of those after
  it move up by one; `observe` to see which is which.
- `--name` takes the whole name or a part of it, in any case. It fails when
  several elements match, and lists them; use a ref then.
- Prefer refs or names. Use positions when the element you need has neither.
- To read what a page says, use `text`, not `observe --all`. In a browser it
  prints only the page: paragraphs as lines, table rows with `|` between
  cells, and the content of an input in `[ ]`.
- Positions are screen pixels, the same as in a `look` image at scale 1.
- `observe` lists interactive elements. Add `--all` for headings and text.
- For a visual check, `look --region x,y,width,height` saves a cropped PNG.
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
- When a dialog covers the page, `observe` still lists the covered elements
  before the dialog's own. Use the dialog's refs if they are clear.
- A dropped connection is not resumed. Run the command again after checking
  with `observe` whether the action already took effect.

## The person watching

For a full-resolution HTML viewer, run `bash-screen web --open` (local Python 3
is required). Without `--open`, it prints a local URL to open in a browser.
It supports zoom, pause, and optional click, scroll and keyboard control.
Use its Send text field for input methods. `Ctrl+]` releases keyboard control;
`Ctrl+C` in the serving terminal stops the viewer and leaves remote apps running.
The page needs a browser surface; a plain terminal cannot render its HTML.

For a character preview, run `BASH_SCREEN_PREVIEW_BLOCKS=quad bash-screen preview`.
This uses widely supported quadrant blocks, also the automatic fallback when
the terminal cannot display images. Smaller terminal fonts give more detail;
`res full` does not increase the character grid. `sextant` offers more vertical
detail but needs font support; switch to `quad` if it shows empty boxes.

Inside herdr or the Herdr desktop app, the first command opens a live preview
pane beside your terminal; that is expected. Set `BASH_SCREEN_NO_PREVIEW=1`
to prevent it. The person can take control of the screen from that pane, and
their actions appear in the action log marked `you:`. `bash-screen vnc`
prints how they can attach a VNC client instead.
