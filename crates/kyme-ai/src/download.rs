//! 下载扩展词库：用户在设置窗口里点了下载才联网。和 AI 预测选词共用这一个联网的 crate，
//! 别的 crate 都不碰网络。

use std::io::Read;
use std::time::Duration;

/// 连不上就换下一个地址，不干等。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 整个下载最多等这么久：十几 MB 的文件，网慢时也够了。
const TIMEOUT: Duration = Duration::from_secs(300);
/// 几个地址轮流试几轮。握手被断开这类问题常常过一会儿就好（国内连 GitHub 尤其常见），试一次就放弃太早。
const ROUNDS: usize = 3;

/// 几个地址轮流试，返回第一个下载成功的内容；都不行就返回最后一次的原因。
/// 边下边把已下载和总共的字节数交给 `progress`（服务器没说总共多少时是 None）。
pub fn download(urls: &[String], progress: impl Fn(u64, Option<u64>)) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TIMEOUT)
        .build()
        .map_err(|err| format!("无法建立网络连接：{err}"))?;
    let mut failure = "没有下载地址".to_string();
    for url in (0..ROUNDS).flat_map(|_| urls) {
        match fetch(&client, url, &progress) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => failure = err,
        }
    }
    Err(failure)
}

fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    progress: &impl Fn(u64, Option<u64>),
) -> Result<Vec<u8>, String> {
    let mut response = client.get(url).send().map_err(|err| super::unreachable(&err))?;
    if !response.status().is_success() {
        return Err(format!("服务器返回 {}", response.status()));
    }
    let total = response.content_length();
    let mut bytes = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let read = response.read(&mut chunk).map_err(|err| format!("下载中断：{err}"))?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
        progress(bytes.len() as u64, total);
    }
}
