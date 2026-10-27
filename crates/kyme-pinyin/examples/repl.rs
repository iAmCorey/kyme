//! 在终端里试引擎，每行一个拼音串：
//! cargo run --release -p kyme-pinyin --example repl -- data/rime-ice/8105.dict.yaml data/rime-ice/base.dict.yaml

use std::io::{self, BufRead, Write};
use std::time::Instant;

use kyme_pinyin::{Dictionary, Engine};

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    let started = Instant::now();
    let dict = match Dictionary::load_rime(&paths) {
        Ok(dict) => dict,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    println!("词库 {} 条，音节 {} 个，载入 {:?}", dict.len(), dict.table().len(), started.elapsed());
    let engine = Engine::new(dict);

    let mut stdout = io::stdout();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        let started = Instant::now();
        let conversion = engine.convert(input);
        let elapsed = started.elapsed();
        let top: Vec<&str> = conversion.candidates.iter().take(6).map(|c| c.text.as_str()).collect();
        // 用了纠错时，打的字母后面再跟上引擎理解成的拼写
        let pinyin = if conversion.spelling == conversion.preedit {
            conversion.preedit.clone()
        } else {
            format!("{} → {}", conversion.preedit, conversion.spelling)
        };
        writeln!(stdout, "{pinyin:<28} {elapsed:>9.2?}  {}", top.join(" ")).ok();
    }
}
