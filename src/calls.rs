//! A record of the commands run against each session, kept on this machine,
//! so that `bash-screen stats` can say where the time of a task went: inside
//! bash-screen, or between its calls, in the agent and the tools that run it.
//! Only the command, its timing and whether it worked are kept, never what
//! was typed.

use crate::config;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE: &str = "calls.jsonl";
/// Past this size the record moves aside to `calls.jsonl.1`, replacing the one before.
const MOST_BYTES: u64 = 1 << 20;
/// A pause longer than this ends a stretch of work.
const BREAK: f64 = 600.0;

/// Seconds since 1970.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub start: f64,
    pub seconds: f64,
    pub command: String,
    /// The steps of a batch; 1 for any other command.
    pub steps: u64,
    pub ok: bool,
}

/// The call this process is making, once it is known to be an agent's.
static CURRENT: Mutex<Option<(String, Call)>> = Mutex::new(None);

/// Note that this process makes a call to `target`, starting now.
pub fn begin(target: &str, command: &str, steps: u64) {
    let call = Call {
        start: now(),
        seconds: 0.0,
        command: command.to_string(),
        steps,
        ok: true,
    };
    *CURRENT.lock().unwrap() = Some((target.to_string(), call));
}

/// Record the call begun in this process, if there is one.
pub fn finish(ok: bool) {
    if let Some((target, mut call)) = CURRENT.lock().unwrap().take() {
        call.seconds = now() - call.start;
        call.ok = ok;
        record(&target, &call);
    }
}

/// Add a call to the record of `target`. A record that cannot be written is
/// not worth failing the command for.
fn record(target: &str, call: &Call) {
    let _ = (|| -> Result<()> {
        let path = config::state_dir()?.join(FILE);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MOST_BYTES) {
            std::fs::rename(&path, path.with_extension("jsonl.1"))?;
        }
        let line = json!({
            "target": target, "start": call.start, "seconds": call.seconds,
            "cmd": call.command, "steps": call.steps, "ok": call.ok,
        });
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{line}")?;
        Ok(())
    })();
}

/// The recorded calls of `target`, oldest first.
fn load(target: &str) -> Result<Vec<Call>> {
    let path = config::state_dir()?.join(FILE);
    let mut calls = Vec::new();
    for file in [path.with_extension("jsonl.1"), path] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if entry["target"].as_str() != Some(target) {
                continue;
            }
            calls.push(Call {
                start: entry["start"].as_f64().unwrap_or(0.0),
                seconds: entry["seconds"].as_f64().unwrap_or(0.0),
                command: entry["cmd"].as_str().unwrap_or("?").to_string(),
                steps: entry["steps"].as_u64().unwrap_or(1),
                ok: entry["ok"].as_bool().unwrap_or(true),
            });
        }
    }
    calls.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(calls)
}

/// The calls to sum up: those of the last `since` seconds, or else the
/// latest stretch that has no pause longer than `BREAK`.
fn chosen(calls: &[Call], since: Option<f64>, now: f64) -> &[Call] {
    let first = match since {
        Some(since) => calls.partition_point(|c| c.start < now - since),
        None => {
            let mut first = calls.len().saturating_sub(1);
            while first > 0 && gap(&calls[first - 1], &calls[first]) <= BREAK {
                first -= 1;
            }
            first
        }
    };
    &calls[first.min(calls.len())..]
}

/// Time from the end of one call to the start of the next. Calls of two
/// agents on one session can overlap; that counts as no time between them.
fn gap(before: &Call, after: &Call) -> f64 {
    (after.start - (before.start + before.seconds)).max(0.0)
}

#[derive(Debug, PartialEq)]
pub struct Summary {
    pub calls: usize,
    pub span: f64,
    pub inside: f64,
    pub between: f64,
    pub longest_gap: f64,
    pub failed: usize,
    /// Per command: how many calls, and how many steps they carried.
    pub commands: Vec<(String, usize, u64)>,
    pub ended: f64,
}

fn summary(calls: &[Call]) -> Option<Summary> {
    let (first, last) = (calls.first()?, calls.last()?);
    let gaps: Vec<f64> = calls.windows(2).map(|w| gap(&w[0], &w[1])).collect();
    let mut commands: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for call in calls {
        let entry = commands.entry(&call.command).or_default();
        entry.0 += 1;
        entry.1 += call.steps;
    }
    let mut commands: Vec<(String, usize, u64)> = commands
        .into_iter()
        .map(|(name, (count, steps))| (name.to_string(), count, steps))
        .collect();
    commands.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Some(Summary {
        calls: calls.len(),
        span: last.start + last.seconds - first.start,
        // A sum of no numbers is -0.0 for f64; start from 0.0 instead.
        inside: calls.iter().fold(0.0, |sum, c| sum + c.seconds),
        between: gaps.iter().fold(0.0, |sum, gap| sum + gap),
        longest_gap: gaps.iter().copied().fold(0.0, f64::max),
        failed: calls.iter().filter(|c| !c.ok).count(),
        commands,
        ended: last.start + last.seconds,
    })
}

fn duration(seconds: f64) -> String {
    let whole = seconds.round() as u64;
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else if whole < 3600 {
        format!("{}m {}s", whole / 60, whole % 60)
    } else {
        format!("{}h {}m", whole / 3600, whole % 3600 / 60)
    }
}

/// Print how the recorded calls of `target` spent their time.
pub fn show(
    target: &str,
    label: &str,
    since_minutes: Option<f64>,
    json_output: bool,
) -> Result<()> {
    let calls = load(target)?;
    let now = now();
    let Some(sum) = summary(chosen(&calls, since_minutes.map(|m| m * 60.0), now)) else {
        if json_output {
            println!("{}", json!({"ok": true, "calls": 0}));
        } else {
            println!("no calls to {label} recorded");
        }
        return Ok(());
    };
    if json_output {
        let commands: Vec<Value> = sum
            .commands
            .iter()
            .map(|(name, count, steps)| json!({"cmd": name, "calls": count, "steps": steps}))
            .collect();
        println!(
            "{}",
            json!({
                "ok": true, "calls": sum.calls, "span": sum.span, "inside": sum.inside,
                "between": sum.between, "longest_gap": sum.longest_gap, "failed": sum.failed,
                "ago": now - sum.ended, "commands": commands,
            })
        );
        return Ok(());
    }
    let between_calls = sum.calls.saturating_sub(1) as f64;
    let commands: Vec<String> = sum
        .commands
        .iter()
        .map(|(name, count, steps)| {
            if *steps > *count as u64 {
                format!("{name} {count} ({steps} steps)")
            } else {
                format!("{name} {count}")
            }
        })
        .collect();
    println!(
        "{} call{} to {label} over {}, the last {} ago",
        sum.calls,
        if sum.calls == 1 { "" } else { "s" },
        duration(sum.span),
        duration(now - sum.ended)
    );
    println!(
        "  in bash-screen  {}, {} a call",
        duration(sum.inside),
        duration(sum.inside / sum.calls as f64)
    );
    if sum.calls > 1 {
        println!(
            "  between calls   {}, {} on average, the longest {}",
            duration(sum.between),
            duration(sum.between / between_calls),
            duration(sum.longest_gap)
        );
    }
    println!("  failed          {}", sum.failed);
    println!("  commands        {}", commands.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(start: f64, seconds: f64, command: &str, steps: u64, ok: bool) -> Call {
        Call {
            start,
            seconds,
            command: command.to_string(),
            steps,
            ok,
        }
    }

    #[test]
    fn the_latest_stretch_ends_at_a_long_pause() {
        let calls = vec![
            call(0.0, 1.0, "observe", 1, true),
            call(2000.0, 0.5, "observe", 1, true),
            call(2010.0, 0.5, "do", 4, true),
            call(2020.0, 1.0, "click", 1, false),
        ];
        let chosen = chosen(&calls, None, 2100.0);
        assert_eq!(chosen.len(), 3);
        let sum = summary(chosen).unwrap();
        assert_eq!(sum.calls, 3);
        assert_eq!(sum.span, 21.0);
        assert_eq!(sum.inside, 2.0);
        assert_eq!(sum.between, 19.0);
        assert_eq!(sum.longest_gap, 9.5);
        assert_eq!(sum.failed, 1);
        assert_eq!(
            sum.commands,
            vec![
                ("click".to_string(), 1, 1),
                ("do".to_string(), 1, 4),
                ("observe".to_string(), 1, 1)
            ]
        );
    }

    #[test]
    fn since_takes_the_calls_of_the_last_minutes() {
        let calls = vec![
            call(0.0, 1.0, "observe", 1, true),
            call(2000.0, 0.5, "observe", 1, true),
        ];
        assert_eq!(chosen(&calls, Some(3000.0), 2100.0).len(), 2);
        assert_eq!(chosen(&calls, Some(60.0), 2100.0).len(), 0);
        assert!(summary(chosen(&[], None, 0.0)).is_none());
    }

    #[test]
    fn overlapping_calls_count_no_time_between() {
        let calls = vec![
            call(0.0, 5.0, "wait", 1, true),
            call(1.0, 0.5, "observe", 1, true),
        ];
        assert_eq!(summary(&calls).unwrap().between, 0.0);
    }
}
