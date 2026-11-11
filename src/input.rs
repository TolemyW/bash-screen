//! Keyboard and mouse input arriving in the preview pane, decoded from the
//! bytes a terminal sends.

use std::io::Read;
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum Input {
    /// Printable text, including pasted text.
    Text(String),
    /// A key or key combination, named the way the remote side expects.
    Key(String),
    /// A mouse button going down. The position is 1-based, in cells or in
    /// pixels depending on what the terminal reports.
    Press {
        button: &'static str,
        x: usize,
        y: usize,
    },
    Wheel {
        up: bool,
        x: usize,
        y: usize,
    },
    /// The terminal's answer to "do you report the mouse in pixels?".
    PixelMouse(bool),
    /// Ctrl+]: stop controlling the remote screen.
    Leave,
}

/// "ctrl+shift+" and so on, from the modifier parameter of a key sequence.
fn modifiers(parameter: usize) -> String {
    let bits = parameter.saturating_sub(1);
    let mut prefix = String::new();
    for (bit, name) in [(4, "ctrl+"), (2, "alt+"), (1, "shift+")] {
        if bits & bit != 0 {
            prefix.push_str(name);
        }
    }
    prefix
}

fn letter_key(letter: u8) -> Option<&'static str> {
    Some(match letter {
        b'A' => "up",
        b'B' => "down",
        b'C' => "right",
        b'D' => "left",
        b'H' => "home",
        b'F' => "end",
        b'P' => "f1",
        b'Q' => "f2",
        b'R' => "f3",
        b'S' => "f4",
        _ => return None,
    })
}

fn tilde_key(number: usize) -> Option<&'static str> {
    Some(match number {
        1 | 7 => "home",
        2 => "insert",
        3 => "delete",
        4 | 8 => "end",
        5 => "pageup",
        6 => "pagedown",
        11 => "f1",
        12 => "f2",
        13 => "f3",
        14 => "f4",
        15 => "f5",
        17 => "f6",
        18 => "f7",
        19 => "f8",
        20 => "f9",
        21 => "f10",
        23 => "f11",
        24 => "f12",
        _ => return None,
    })
}

const PASTE_END: &[u8] = b"\x1b[201~";

/// Decode everything in `bytes`. Sequences that mean nothing here (focus
/// reports, mouse releases and motion) are dropped.
pub fn parse(bytes: &[u8]) -> Vec<Input> {
    let mut out: Vec<Input> = Vec::new();
    let push_text = |out: &mut Vec<Input>, text: &[u8]| {
        let text = String::from_utf8_lossy(text).replace('\r', "\n");
        match out.last_mut() {
            Some(Input::Text(previous)) => previous.push_str(&text),
            _ => out.push(Input::Text(text)),
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            0x1b if bytes.get(i + 1) == Some(&b'[') => {
                // Control sequence: parameters, intermediates, one final byte.
                let start = i + 2;
                let mut end = start;
                while end < bytes.len() && (0x20..=0x3f).contains(&bytes[end]) {
                    end += 1;
                }
                let Some(&last) = bytes.get(end) else {
                    break;
                };
                let body = String::from_utf8_lossy(&bytes[start..end]).to_string();
                i = end + 1;
                let numbers: Vec<usize> = body
                    .trim_start_matches(['<', '?'])
                    .trim_end_matches('$')
                    .split(';')
                    .map(|n| n.parse().unwrap_or(0))
                    .collect();
                let number = |index: usize| numbers.get(index).copied().unwrap_or(0);
                if body.starts_with('<') {
                    // Mouse report: buttons;x;y, 'M' for press and 'm' for release.
                    let (code, x, y) = (number(0), number(1), number(2));
                    if last != b'M' || code & 32 != 0 {
                        continue;
                    }
                    if code & 64 != 0 {
                        match code & 3 {
                            0 => out.push(Input::Wheel { up: true, x, y }),
                            1 => out.push(Input::Wheel { up: false, x, y }),
                            _ => {}
                        }
                    } else {
                        let button = match code & 3 {
                            0 => "left",
                            1 => "middle",
                            2 => "right",
                            _ => continue,
                        };
                        out.push(Input::Press { button, x, y });
                    }
                } else if body.starts_with('?') {
                    if last == b'y' && number(0) == 1016 {
                        out.push(Input::PixelMouse(matches!(number(1), 1 | 3)));
                    }
                } else if last == b'~' && number(0) == 200 {
                    // Bracketed paste: everything up to the end marker is text.
                    let rest = &bytes[i..];
                    let length = rest
                        .windows(PASTE_END.len())
                        .position(|window| window == PASTE_END)
                        .unwrap_or(rest.len());
                    push_text(&mut out, &rest[..length]);
                    i += (length + PASTE_END.len()).min(rest.len());
                } else if last == b'~' {
                    if let Some(name) = tilde_key(number(0)) {
                        out.push(Input::Key(format!("{}{name}", modifiers(number(1)))));
                    }
                } else if last == b'Z' {
                    out.push(Input::Key("shift+tab".into()));
                } else if let Some(name) = letter_key(last) {
                    out.push(Input::Key(format!("{}{name}", modifiers(number(1)))));
                }
            }
            0x1b if bytes.get(i + 1) == Some(&b'O') => {
                if let Some(name) = bytes.get(i + 2).copied().and_then(letter_key) {
                    out.push(Input::Key(name.into()));
                }
                i += 3;
            }
            0x1b => match bytes.get(i + 1).copied() {
                None => {
                    out.push(Input::Key("escape".into()));
                    i += 1;
                }
                // Escape followed by a character is that character with Alt.
                Some(next) => {
                    if next.is_ascii_graphic() {
                        out.push(Input::Key(format!(
                            "alt+{}",
                            (next as char).to_ascii_lowercase()
                        )));
                    } else if next == 0x7f {
                        out.push(Input::Key("alt+backspace".into()));
                    }
                    i += 2;
                }
            },
            0x1d => {
                out.push(Input::Leave);
                i += 1;
            }
            0x0d | 0x0a => {
                out.push(Input::Key("return".into()));
                i += 1;
            }
            0x09 => {
                out.push(Input::Key("tab".into()));
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(Input::Key("backspace".into()));
                i += 1;
            }
            0x00 => {
                out.push(Input::Key("ctrl+space".into()));
                i += 1;
            }
            0x01..=0x1a => {
                out.push(Input::Key(format!("ctrl+{}", (b'a' + byte - 1) as char)));
                i += 1;
            }
            0x1c..=0x1f => i += 1,
            _ => {
                let start = i;
                while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] != 0x7f {
                    i += 1;
                }
                push_text(&mut out, &bytes[start..i]);
            }
        }
    }
    out
}

fn readable(timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as i32) > 0 }
}

/// Wait up to `timeout` for input, then return everything that has arrived.
pub fn read(timeout: Duration) -> Option<Vec<u8>> {
    if !readable(timeout) {
        return None;
    }
    let mut all = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match std::io::stdin().read(&mut buffer) {
            Ok(n) if n > 0 => all.extend_from_slice(&buffer[..n]),
            // End of input (no terminal): behave like a plain wait.
            _ => {
                if all.is_empty() {
                    std::thread::sleep(timeout);
                    return None;
                }
                break;
            }
        }
        // Give the rest of an escape sequence or a paste a moment to arrive.
        if all.len() > 1 << 16 || !readable(Duration::from_millis(10)) {
            break;
        }
    }
    Some(all)
}

/// Deliver key presses at once and without echo. When controlling the remote
/// screen every key belongs to it, so Ctrl-C and friends are passed through
/// instead of acting on this process.
pub fn set_terminal(control: bool) {
    unsafe {
        let mut mode: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut mode) != 0 {
            return;
        }
        mode.c_lflag &= !(libc::ICANON | libc::ECHO);
        if control {
            mode.c_lflag &= !(libc::ISIG | libc::IEXTEN);
            mode.c_iflag &= !(libc::ICRNL | libc::IXON);
        } else {
            mode.c_lflag |= libc::ISIG | libc::IEXTEN;
            mode.c_iflag |= libc::ICRNL | libc::IXON;
        }
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &mode);
    }
}

/// Ask the terminal for mouse presses (in pixels where it can) and for pastes
/// to arrive marked, then ask whether pixel reporting took effect.
pub const ENTER_CONTROL: &str = "\x1b[?1000h\x1b[?1006h\x1b[?1016h\x1b[?2004h\x1b[?1016$p";
pub const LEAVE_CONTROL: &str = "\x1b[?1016l\x1b[?1006l\x1b[?1000l\x1b[?2004l";

#[cfg(test)]
mod tests {
    use super::{parse, Input};

    fn key(name: &str) -> Input {
        Input::Key(name.into())
    }

    #[test]
    fn text_and_keys_keep_their_order() {
        assert_eq!(
            parse("ab你\x7f\r".as_bytes()),
            vec![Input::Text("ab你".into()), key("backspace"), key("return")]
        );
    }

    #[test]
    fn special_keys_and_modifiers() {
        assert_eq!(
            parse(b"\x1b[A\x1b[3~\x1b[1;5C\x1b[Z"),
            vec![
                key("up"),
                key("delete"),
                key("ctrl+right"),
                key("shift+tab")
            ]
        );
        assert_eq!(
            parse(b"\x01\x1bf\x1b"),
            vec![key("ctrl+a"), key("alt+f"), key("escape")]
        );
    }

    #[test]
    fn mouse_presses_and_wheel_but_not_releases_or_motion() {
        assert_eq!(
            parse(b"\x1b[<0;10;5M\x1b[<0;10;5m\x1b[<32;11;5M\x1b[<2;7;8M\x1b[<65;3;4M"),
            vec![
                Input::Press {
                    button: "left",
                    x: 10,
                    y: 5
                },
                Input::Press {
                    button: "right",
                    x: 7,
                    y: 8
                },
                Input::Wheel {
                    up: false,
                    x: 3,
                    y: 4
                },
            ]
        );
    }

    #[test]
    fn a_paste_is_text_even_when_it_contains_control_bytes() {
        assert_eq!(
            parse(b"\x1b[200~a\x1b[Ab\rc\x1b[201~x"),
            vec![Input::Text("a\x1b[Ab\ncx".into())]
        );
    }

    #[test]
    fn leave_and_the_pixel_mouse_answer() {
        assert_eq!(
            parse(b"\x1d\x1b[?1016;1$y\x1b[?1016;0$y\x1b[I"),
            vec![
                Input::Leave,
                Input::PixelMouse(true),
                Input::PixelMouse(false)
            ]
        );
    }
}
