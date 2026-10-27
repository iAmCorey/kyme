//! macOS 接入层：用 `Kyme --qa`（自测模式，见 crates/kyme-macos/src/qa.rs）跑 qa/scenarios.tsv。
//!
//! 自测模式里是真正的输入会话、macOS 接入代码和候选窗，只把系统的输入法服务换成了一个假的输入框。
//! 每个键都和本地的参照会话对照：这个键接没接、上屏了什么、输入框里正在输入的文字、候选窗是否贴在光标旁；
//! 最后看按键处理耗时和内存。不切输入法，不抢焦点，随时可以跑。
//!
//! 为什么不在真的 App 里模拟打字：程序合成的字母键经系统转给输入法时，输入法收到的是控制字符，
//! 认不出是哪个字母（真实键盘没有这个问题）。各个 App 里的表现交给人工检查清单（qa/manual.toml）。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use kyme_core::{Session, View};
use serde::{Deserialize, Serialize};

use crate::report::Outcome;
use crate::scenario::{self, Press, Scenario, Stroke};
use crate::{Context, util};

/// 还没选定任何字时，输入框里放的占位符（只在拼音不显示在输入框里时用到），和 kyme-macos 的 state.rs 一致。
const PLACEHOLDER: &str = "\u{3000}";
/// 中英提示显示多久（state.rs 里是 700ms）。等它消失了再检查候选窗有没有收起来。
const HINT: Duration = Duration::from_millis(800);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// 发给 `Kyme --qa` 的指令，和 crates/kyme-macos/src/qa.rs 里的一致。
#[derive(Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Key { flags_changed: bool, code: u16, chars: String, shift: bool, command: bool },
    Clear,
    State,
}

/// `Kyme --qa` 的回复，和 crates/kyme-macos/src/qa.rs 里的一致。坐标原点在主屏左上角。
#[derive(Deserialize)]
struct State {
    handled: bool,
    inserted: String,
    marked: String,
    caret: [f64; 4],
    panel: Option<[f64; 4]>,
    elapsed_us: u64,
}

pub fn run(ctx: &mut Context) -> Outcome {
    test(ctx).unwrap_or_else(Outcome::fail)
}

fn test(ctx: &mut Context) -> Result<Outcome, String> {
    let loaded = ctx.engine()?;
    let scenarios = scenario::load(&ctx.root)?;
    util::capture(Command::new(ctx.root.join("scripts/build-app.sh")).current_dir(&ctx.root))
        .map_err(|out| format!("组装 Kyme.app 失败：{}", util::tail(&out, 3).join(" / ")))?;
    let mut kyme = Kyme::spawn(&ctx.root.join("target/Kyme.app/Contents/MacOS/Kyme"))?;

    // 自测模式用默认设置，参照也一样
    let mut oracle = Session::new(loaded.engine);
    let mut failures = Vec::new();
    let mut timings = Vec::new();
    for scenario in &scenarios {
        if let Err(problem) = play(&mut kyme, &mut oracle, scenario, &mut timings)? {
            failures.push(format!("{}：{problem}", scenario.name));
        }
    }
    let rss = kyme.rss_mb();
    drop(kyme);

    let gates = &ctx.gates.macos;
    let mut details = Vec::new();
    timings.sort();
    let at = |q: f64| timings.get(((timings.len().saturating_sub(1)) as f64 * q).round() as usize).copied();
    if let (Some(p95), Some(slowest)) = (at(0.95), at(1.0)) {
        details.push(format!(
            "按键处理 p95 {}（门槛 {}ms），最慢一次 {}，共 {} 次",
            util::millis(p95),
            gates.max_key_p95_ms,
            util::millis(slowest),
            timings.len()
        ));
        if util::as_ms(p95) > gates.max_key_p95_ms {
            failures.push(format!("按键处理 p95 {}，超过门槛 {}ms", util::millis(p95), gates.max_key_p95_ms));
        }
    }
    if let Some(rss) = rss {
        details.push(format!("常驻内存 {rss:.0}MB（门槛 {}MB）", gates.max_rss_mb));
        if rss > gates.max_rss_mb {
            failures.push(format!("常驻内存 {rss:.0}MB，超过门槛 {}MB", gates.max_rss_mb));
        }
    }
    let summary = format!("{} 个场景、{} 次按键逐键对照", scenarios.len(), timings.len());
    if failures.is_empty() {
        Ok(Outcome::pass(summary).with_details(details))
    } else {
        failures.extend(details);
        Ok(Outcome::fail(summary).with_details(failures))
    }
}

/// 跑一个场景。外层的 Err 是自测进程出了问题，整个停下；内层的 Err 是这个场景对不上。
fn play(
    kyme: &mut Kyme,
    oracle: &mut Session,
    scenario: &Scenario,
    timings: &mut Vec<Duration>,
) -> Result<Result<(), String>, String> {
    kyme.request(&Request::Clear)?;
    let mut inserted = String::new();
    let mut toggled = None;
    let mut mismatch = None;
    let mut step = 0;
    for stroke in &scenario.strokes {
        let press = match stroke {
            // 自测进程收到下一个键时才看时间，这里照常等
            Stroke::Wait(duration) => {
                thread::sleep(*duration);
                continue;
            }
            Stroke::Press(press) => press,
        };
        step += 1;
        let state = kyme.request(&key(press))?;
        timings.push(Duration::from_micros(state.elapsed_us));
        let response = oracle.handle_key(press.key, press.mods);
        if response.mode_changed.is_some() {
            toggled = Some(Instant::now());
        }
        if let Some(text) = &response.commit {
            inserted.push_str(text);
        }
        // 修饰键的变化总是交还给 App，和 state.rs 一致
        let handled = response.handled && !press.flags_changed;
        if let Err(problem) = compare(&state, handled, &inserted, oracle) {
            mismatch = Some(format!("第 {step} 个键 {} 之后：{problem}", press.label));
            break;
        }
    }
    // 两边都按一次 Esc 回到干净的状态；就算上面对不上，后面的场景也接着跑
    kyme.request(&key(&scenario::escape()))?;
    if let Err(problem) = scenario::finish(oracle) {
        return Ok(Err(problem));
    }
    if let Some(mismatch) = mismatch {
        return Ok(Err(mismatch));
    }
    if let Some(at) = toggled {
        thread::sleep(HINT.saturating_sub(at.elapsed()));
    }
    Ok(match kyme.request(&Request::State)?.panel {
        Some(panel) => Err(format!("输入结束后候选窗还在 {}", show(panel))),
        None => Ok(()),
    })
}

/// 一个键之后，自测进程的状态要和参照一致。
fn compare(state: &State, handled: bool, inserted: &str, oracle: &Session) -> Result<(), String> {
    if state.handled != handled {
        let who = if handled {
            "应该由输入法接住，却交给了 App"
        } else {
            "应该交给 App，却被输入法接住了"
        };
        return Err(who.into());
    }
    if state.inserted != inserted {
        return Err(format!("上屏的是「{}」，应该是「{inserted}」", state.inserted));
    }
    match oracle.view() {
        View::Idle if state.marked.is_empty() => Ok(()),
        View::Idle => Err(format!("已经不在输入了，输入框里还留着「{}」", state.marked)),
        View::Composing { marked, .. } => {
            let expected = if marked.is_empty() { PLACEHOLDER } else { marked.as_str() };
            if state.marked != expected {
                return Err(format!("输入框里正在输入的是「{}」，应该是「{expected}」", state.marked));
            }
            let panel = state.panel.ok_or("正在输入，候选窗却没显示")?;
            if !beside(panel, state.caret) {
                return Err(format!("候选窗在 {}，离光标 {} 太远", show(panel), show(state.caret)));
            }
            Ok(())
        }
    }
}

/// 候选窗贴在光标那一行下方（下方放不下时在上方），左边和光标大致对齐。坐标原点在左上角。
fn beside(panel: [f64; 4], caret: [f64; 4]) -> bool {
    let [px, py, _, ph] = panel;
    let [cx, cy, _, ch] = caret;
    let below = (cy + ch - 8.0..=cy + ch + 40.0).contains(&py);
    let above = (cy - 40.0..=cy + 8.0).contains(&(py + ph));
    (below || above) && (px - cx).abs() <= 60.0
}

fn show([x, y, w, h]: [f64; 4]) -> String {
    format!("({x:.0}, {y:.0}, {w:.0}×{h:.0})")
}

fn key(press: &Press) -> Request {
    Request::Key {
        flags_changed: press.flags_changed,
        code: press.code,
        chars: press.chars.clone(),
        shift: press.shift,
        command: press.command,
    }
}

/// `Kyme --qa` 进程。
struct Kyme {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<String>,
}

impl Kyme {
    fn spawn(path: &std::path::Path) -> Result<Kyme, String> {
        let mut child = Command::new(path)
            .arg("--qa")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|err| format!("拉不起 {}：{err}", path.display()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("拿不到自测进程的标准输入输出".into());
        };
        let (sender, replies) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Kyme { child, stdin, replies })
    }

    fn request(&mut self, request: &Request) -> Result<State, String> {
        let line = serde_json::to_string(request).map_err(|err| err.to_string())?;
        writeln!(self.stdin, "{line}").and_then(|()| self.stdin.flush()).map_err(|_| "自测进程已经退出".to_string())?;
        let line = self.replies.recv_timeout(REPLY_TIMEOUT).map_err(|_| "自测进程没有响应，可能崩溃了".to_string())?;
        let reply: Result<State, String> =
            serde_json::from_str(&line).map_err(|err| format!("看不懂自测进程的回复：{err}"))?;
        reply
    }

    fn rss_mb(&self) -> Option<f64> {
        let pid = self.child.id().to_string();
        let kb: f64 = util::stdout_of(Command::new("ps").args(["-o", "rss=", "-p", &pid]))?.parse().ok()?;
        Some(kb / 1024.0)
    }
}

impl Drop for Kyme {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_must_hug_the_caret() {
        let caret = [100.0, 300.0, 2.0, 24.0];
        assert!(beside([98.0, 328.0, 300.0, 66.0], caret), "下方 4pt");
        assert!(beside([98.0, 230.0, 300.0, 66.0], caret), "放不下时在上方");
        assert!(!beside([600.0, 328.0, 300.0, 66.0], caret), "横向离得太远");
        assert!(!beside([98.0, 500.0, 300.0, 66.0], caret), "纵向离得太远");
    }
}
