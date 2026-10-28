//! AI 预测选词：把光标前的文字和引擎算出的几个整句交给模型，让它挑接得最顺的那一句。模型只能从引擎的整句里挑，
//! 所以挑出来的读音一定和打的拼音对得上。
//!
//! 服务商在设置里选（[`AiProvider`]），按接口分三种：
//! - TypeSafe 的 Jev（文档在 https://docs.typesafe.ai）：不生成文字，只在给定的选项里挑一个，并给出把握有多大。
//! - Anthropic 的 Claude，和 OpenAI 兼容的接口（OpenAI、DeepSeek、通义千问、Kimi、智谱、OpenRouter，还有自定义的）：
//!   聊天模型。把整句编上号，让它只回编号，拿不准时回 1，也就是引擎的首选。它们不给把握度，一律当作有把握。

use std::fmt;
use std::time::{Duration, Instant};

use kyme_core::{AiProvider, AiService};
use reqwest::blocking::RequestBuilder;
use serde_json::{Map, Value, json};

/// 等这么久还没回就算了：AI 只是锦上添花，打字不等它。
const TIMEOUT: Duration = Duration::from_secs(3);
/// TypeSafe 的问题名，回复里按它找答案。
const QUESTION: &str = "next";
/// Anthropic 的 API 版本。
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Claude 挑候选时想多深：挑一个编号用不着多想，想多了还慢。
const ANTHROPIC_EFFORT: &str = "low";
/// Claude 回复的长度上限，思考也算在里面。
const ANTHROPIC_MAX_TOKENS: u32 = 1024;
/// 默认的 Claude。被安全策略拒绝时让 Anthropic 换一个模型接着答（fallbacks），只在它上面开：别的模型不一定支持。
const CLAUDE: &str = "claude-opus-5-5";
const ANTHROPIC_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// 聊天模型的系统提示。
const SYSTEM: &str = "你是中文拼音输入法的选词助手。用户在光标前已经写了一些字，接着用拼音输入法打了一串拼音，\
                      下面列出这串拼音能打出的几句。挑出接在前文后面最通顺、最可能是用户想打的那一句，\
                      只回答它的编号，不要别的字。拿不准就回答 1。";

/// 三种接口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Api {
    TypeSafe,
    OpenAi,
    Anthropic,
}

impl Api {
    /// 接口地址后面要接的路径。
    fn route(self) -> &'static str {
        match self {
            Api::TypeSafe => "/v1/systemone",
            Api::OpenAi => "/chat/completions",
            Api::Anthropic => "/v1/messages",
        }
    }
}

/// 一家服务商：用哪种接口、接口地址、能选的模型。
pub struct Preset {
    api: Api,
    /// 接口地址。自定义的是 None，自己填。
    pub endpoint: Option<&'static str>,
    /// 能选的模型，第一个是默认的。自定义的是空的，自己填。各家常换新模型，设置里也可以填别的。
    pub models: &'static [&'static str],
}

impl Preset {
    /// 自己填地址的接口可以不要 Key：本机跑的 Ollama 之类用不着。
    pub fn key_optional(&self) -> bool {
        self.endpoint.is_none()
    }

    /// 设置里填的模型 `model` 实际用哪个：留空用第一个。自定义的留空就是没有。
    pub fn model<'a>(&self, model: &'a str) -> Option<&'a str> {
        Some(model.trim()).filter(|model| !model.is_empty()).or(self.models.first().copied())
    }
}

pub fn preset(provider: AiProvider) -> Preset {
    let (api, endpoint, models): (Api, Option<&'static str>, &'static [&'static str]) = match provider {
        // 用定死的版本：把握度的门槛是对着这个版本调的，别名换了版本也不会悄悄变
        AiProvider::TypeSafe => (Api::TypeSafe, Some("https://api.typesafe.ai"), &["jev-1.14.0"]),
        AiProvider::Anthropic => {
            (Api::Anthropic, Some("https://api.anthropic.com"), &[CLAUDE, "claude-sonnet-5-5", "claude-haiku-5-5"])
        }
        AiProvider::OpenAi => {
            (Api::OpenAi, Some("https://api.openai.com/v1"), &["gpt-4.1-mini", "gpt-4.1", "gpt-4o-mini"])
        }
        AiProvider::DeepSeek => (Api::OpenAi, Some("https://api.deepseek.com"), &["deepseek-chat"]),
        AiProvider::Qwen => (
            Api::OpenAi,
            Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            &["qwen-plus", "qwen-turbo", "qwen-max"],
        ),
        AiProvider::Kimi => {
            (Api::OpenAi, Some("https://api.moonshot.cn/v1"), &["kimi-k2-turbo-preview", "moonshot-v1-8k"])
        }
        AiProvider::Zhipu => {
            (Api::OpenAi, Some("https://open.bigmodel.cn/api/paas/v4"), &["glm-4-flash", "glm-4.5-air"])
        }
        AiProvider::OpenRouter => {
            (Api::OpenAi, Some("https://openrouter.ai/api/v1"), &["deepseek/deepseek-chat", "openai/gpt-4.1-mini"])
        }
        AiProvider::Custom => (Api::OpenAi, None, &[]),
    };
    Preset { api, endpoint, models }
}

/// 请求发到哪：接口地址接上路径。填的已经是完整地址（带着这段路径）就不再接。
fn url(api: Api, endpoint: &str) -> String {
    let base = endpoint.trim().trim_end_matches('/');
    if base.ends_with(api.route()) { base.to_string() } else { format!("{base}{}", api.route()) }
}

/// 问模型时出的错。说明里不带打的字，可以写进日志。
#[derive(Debug, PartialEq)]
pub enum Error {
    /// Key 不对、被停用了或没有权限（401、403）。换了 Key 才可能好。
    Key,
    /// 服务商回了错误。`reason` 是它自己的说明（模型名不对、额度用完之类），有的会带着发过去的字，
    /// 只给人看，不写日志（见 `log_line`）。
    Status { status: u16, reason: String },
    /// 网络不通、超时、请求太多、回复看不懂之类。说明是这里自己写的，不带打的字。
    Other(String),
}

impl Error {
    /// 写日志用的说明：不带服务商的原话，日志里不能有打的字。
    pub fn log_line(&self) -> String {
        match self {
            Error::Status { status, .. } => format!("请求失败（HTTP {status}）"),
            other => other.to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Key => f.write_str("API Key 无效"),
            Error::Status { status, reason } => write!(f, "请求失败（HTTP {status}）{reason}"),
            Error::Other(text) => f.write_str(text),
        }
    }
}

/// 模型挑的结果。
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// 挑中的是第几个候选。
    pub index: usize,
    /// 把握度，0 到 1：最大的那个概率比平均分出来的高多少，选项越多同样的概率把握度越高。
    /// 只有 TypeSafe 给，聊天模型一律是 1。
    pub confidence: f64,
    /// 这次用了多少输入 token。
    pub input_tokens: u64,
}

pub struct Client {
    http: reqwest::blocking::Client,
    provider: AiProvider,
    api: Api,
    url: String,
    model: String,
    /// 自定义的接口可以不要 Key（本机跑的 Ollama 之类），这时是空的，请求里不带。
    key: String,
}

impl Client {
    /// 按设置里这一家的 Key、模型（和自定义的接口地址）建好。该填的没填、地址不对时返回说明。
    pub fn new(provider: AiProvider, service: &AiService) -> Result<Self, String> {
        let preset = preset(provider);
        let key = service.key.trim();
        if key.is_empty() && !preset.key_optional() {
            return Err("未填写 API Key".into());
        }
        let model = preset.model(&service.model).ok_or("未填写模型")?;
        let endpoint = preset.endpoint.unwrap_or(service.endpoint.trim());
        if endpoint.is_empty() {
            return Err("未填写接口地址".into());
        }
        if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
            return Err("接口地址应以 http:// 或 https:// 开头".into());
        }
        let http = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| format!("无法创建 HTTP 客户端：{err}"))?;
        let url = url(preset.api, endpoint);
        Ok(Client { http, provider, api: preset.api, url, model: model.into(), key: key.into() })
    }

    pub fn provider(&self) -> AiProvider {
        self.provider
    }

    /// 用的是哪个模型。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 光标前是 `context`，接着打了拼音 `pinyin`，在 `candidates` 里挑接得最顺的一个。
    /// 还返回从发出请求到收到回复用了多久。
    pub fn pick(&self, context: &str, pinyin: &str, candidates: &[&str]) -> Result<(Pick, Duration), Error> {
        let started = Instant::now();
        let response =
            self.request(context, pinyin, candidates).send().map_err(|err| Error::Other(unreachable(&err)))?;
        let status = response.status().as_u16();
        match status {
            401 | 403 => return Err(Error::Key),
            429 | 529 => return Err(Error::Other("请求过多，请稍后再试".into())),
            _ => {}
        }
        let reply: Value = response.json().map_err(|_| Error::Other(format!("返回内容无法解析（HTTP {status}）")))?;
        let elapsed = started.elapsed();
        if status != 200 {
            return Err(Error::Status { status, reason: reason(&reply) });
        }
        let pick = match self.api {
            Api::TypeSafe => parse_typesafe(&reply, candidates),
            Api::OpenAi => parse_openai(&reply, candidates.len()),
            Api::Anthropic => parse_anthropic(&reply, candidates.len()),
        };
        pick.map(|pick| (pick, elapsed)).map_err(Error::Other)
    }

    fn request(&self, context: &str, pinyin: &str, candidates: &[&str]) -> RequestBuilder {
        let post = self.http.post(&self.url);
        let bearer = |post: RequestBuilder| if self.key.is_empty() { post } else { post.bearer_auth(&self.key) };
        match self.api {
            Api::TypeSafe => bearer(post).json(&typesafe_request(&self.model, context, pinyin, candidates)),
            Api::OpenAi => bearer(post).json(&openai_request(&self.model, context, pinyin, candidates)),
            Api::Anthropic => {
                let fallback = self.model == CLAUDE;
                let post = post.header("x-api-key", &self.key).header("anthropic-version", ANTHROPIC_VERSION);
                let post = if fallback { post.header("anthropic-beta", ANTHROPIC_FALLBACK_BETA) } else { post };
                post.json(&anthropic_request(&self.model, fallback, context, pinyin, candidates))
            }
        }
    }
}

/// TypeSafe：光标前的文字是 state，候选是一道选择题的选项。选项的名字就是候选的文字，模型直接看得到。
fn typesafe_request(model: &str, context: &str, pinyin: &str, candidates: &[&str]) -> Value {
    let criteria: Map<String, Value> = candidates.iter().map(|text| (text.to_string(), Value::Null)).collect();
    let instructions =
        format!("这是用户光标前已经写好的文字。用户接着用拼音输入法打了「{pinyin}」，下面哪一个最可能是用户想打的？");
    json!({
        "model": model,
        "state": context,
        "questions": {
            QUESTION: { "type": "choice", "instructions": instructions, "criteria": criteria },
        },
    })
}

/// 聊天模型看到的那段话：前文、拼音，和编了号的整句。
fn chat_prompt(context: &str, pinyin: &str, candidates: &[&str]) -> String {
    let mut prompt = format!("前文：{context}\n拼音：{pinyin}\n");
    for (i, candidate) in candidates.iter().enumerate() {
        prompt.push_str(&format!("{}. {candidate}\n", i + 1));
    }
    prompt
}

/// OpenAI 兼容的接口。不限回复长度：有的模型不认 max_tokens，回编号本来也就几个字。
fn openai_request(model: &str, context: &str, pinyin: &str, candidates: &[&str]) -> Value {
    json!({
        "model": model,
        "temperature": 0,
        "messages": [
            { "role": "system", "content": SYSTEM },
            { "role": "user", "content": chat_prompt(context, pinyin, candidates) },
        ],
    })
}

/// Anthropic。新的 API 版本不收 temperature，想多深用 effort 定。
fn anthropic_request(model: &str, fallback: bool, context: &str, pinyin: &str, candidates: &[&str]) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "system": SYSTEM,
        "messages": [{ "role": "user", "content": chat_prompt(context, pinyin, candidates) }],
        "output_config": { "effort": ANTHROPIC_EFFORT },
    });
    if fallback {
        body["fallbacks"] = json!("default");
    }
    body
}

/// TypeSafe 的回复：挑中的选项和把握度。挑了候选以外的东西算出错。
fn parse_typesafe(reply: &Value, candidates: &[&str]) -> Result<Pick, String> {
    let answer = &reply["answers"][QUESTION];
    let chosen = answer["choice"].as_str().ok_or("返回结果里没有选项")?;
    let index = candidates.iter().position(|text| *text == chosen).ok_or("返回的选项不在候选中")?;
    Ok(Pick {
        index,
        confidence: answer["confidence"].as_f64().unwrap_or(0.0),
        input_tokens: reply["usage"]["input_tokens"].as_u64().unwrap_or(0),
    })
}

fn parse_openai(reply: &Value, count: usize) -> Result<Pick, String> {
    let text = reply["choices"][0]["message"]["content"].as_str().ok_or("返回结果为空")?;
    let index = chosen_number(text, count)?;
    Ok(Pick { index, confidence: 1.0, input_tokens: reply["usage"]["prompt_tokens"].as_u64().unwrap_or(0) })
}

/// Anthropic 的回复：前面可能有思考块，取文字块；被拒绝了就没有答案。
fn parse_anthropic(reply: &Value, count: usize) -> Result<Pick, String> {
    if reply["stop_reason"] == "refusal" {
        return Err("模型拒绝了请求".into());
    }
    let text = reply["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find(|block| block["type"] == "text"))
        .and_then(|block| block["text"].as_str())
        .ok_or("返回结果为空")?;
    let index = chosen_number(text, count)?;
    Ok(Pick { index, confidence: 1.0, input_tokens: reply["usage"]["input_tokens"].as_u64().unwrap_or(0) })
}

/// 聊天模型回的编号（从 1 起），换成下标。取回复里的第一个数。
fn chosen_number(text: &str, count: usize) -> Result<usize, String> {
    let digits: String = text.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    let number: usize = digits.parse().map_err(|_| "返回结果里没有编号".to_string())?;
    (1..=count).contains(&number).then(|| number - 1).ok_or_else(|| format!("返回的编号 {number} 超出范围"))
}

/// 连不上时的说明：超时就说超时，别的带上最底层的原因（域名解析失败、证书不对……），
/// reqwest 自己的说明里只有「发送请求出错」。
fn unreachable(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "连接超时".into();
    }
    let root = std::iter::successors(Some(err as &dyn std::error::Error), |err| err.source()).last();
    format!("无法连接服务器：{}", root.map(ToString::to_string).unwrap_or_default())
}

/// 出错的回复里说的原因（模型名不对、额度用完之类），前面带冒号。只取一句说明：整个回复可能带着发过去的字。
fn reason(reply: &Value) -> String {
    let message = reply["error"]["message"].as_str().or(reply["detail"].as_str()).or(reply["message"].as_str());
    message.map(|message| format!("：{}", message.chars().take(160).collect::<String>())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANDIDATES: [&str; 2] = ["好难和", "好难喝"];

    fn service(key: &str, model: &str, endpoint: &str) -> AiService {
        AiService { key: key.into(), model: model.into(), endpoint: endpoint.into() }
    }

    #[test]
    fn settings_become_a_client() {
        let client = Client::new(AiProvider::TypeSafe, &service(" ts-key ", "", "")).unwrap();
        assert_eq!((client.model(), client.url.as_str()), ("jev-1.14.0", "https://api.typesafe.ai/v1/systemone"));
        assert_eq!(client.key, "ts-key");
        let client = Client::new(AiProvider::DeepSeek, &service("sk", "", "http://ignored")).unwrap();
        assert_eq!(client.model(), "deepseek-chat");
        assert_eq!(client.url, "https://api.deepseek.com/chat/completions", "预设的地址照用，不看填的");
        assert_eq!(Client::new(AiProvider::Qwen, &service("sk", "qwen3-max", "")).unwrap().model(), "qwen3-max");
        assert_eq!(Client::new(AiProvider::Anthropic, &service("sk", "", "")).unwrap().model(), "claude-opus-5-5");
        let full = "http://localhost:11434/v1/chat/completions";
        let local = Client::new(AiProvider::Custom, &service("", "qwen3", full)).unwrap();
        assert_eq!((local.url.as_str(), local.key.as_str()), (full, ""), "自定义的照填的地址，可以不要 Key");
        assert!(Client::new(AiProvider::TypeSafe, &service("", "", "")).is_err(), "没填 Key");
        assert!(Client::new(AiProvider::Custom, &service("", "", "http://localhost:11434/v1")).is_err(), "没填模型");
        assert!(Client::new(AiProvider::Custom, &service("", "qwen3", "")).is_err(), "没填地址");
        assert!(Client::new(AiProvider::Custom, &service("", "qwen3", "localhost:11434")).is_err(), "地址不对");
    }

    #[test]
    fn every_provider_has_somewhere_to_send() {
        for provider in AiProvider::ALL {
            let preset = preset(provider);
            assert_eq!(preset.endpoint.is_none(), provider == AiProvider::Custom, "{provider:?}");
            assert_eq!(preset.models.is_empty(), provider == AiProvider::Custom, "{provider:?}");
        }
    }

    #[test]
    fn asks_typesafe_a_choice_question_over_the_candidates() {
        let body = typesafe_request("jev-1.14.0", "这杯咖啡又苦又酸，真是", "haonanhe", &CANDIDATES);
        assert_eq!(body["model"], "jev-1.14.0");
        assert_eq!(body["state"], "这杯咖啡又苦又酸，真是");
        let question = &body["questions"][QUESTION];
        assert_eq!(question["type"], "choice");
        assert!(question["instructions"].as_str().unwrap().contains("haonanhe"));
        assert_eq!(question["criteria"], json!({ "好难和": null, "好难喝": null }));
    }

    #[test]
    fn asks_chat_models_for_a_number() {
        let prompt = chat_prompt("真是", "haonanhe", &CANDIDATES);
        assert_eq!(prompt, "前文：真是\n拼音：haonanhe\n1. 好难和\n2. 好难喝\n");
        let body = openai_request("deepseek-chat", "真是", "haonanhe", &CANDIDATES);
        assert_eq!(body["messages"][0]["content"], SYSTEM);
        assert_eq!(body["messages"][1]["content"], prompt);
        let body = anthropic_request("claude-opus-5-5", true, "真是", "haonanhe", &CANDIDATES);
        assert_eq!(body["system"], SYSTEM);
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("temperature").is_none(), "新的 API 版本不收");
        assert!(anthropic_request("claude-haiku-5-5", false, "", "", &CANDIDATES).get("fallbacks").is_none());
    }

    #[test]
    fn reads_the_chosen_candidate() {
        let reply = json!({
            "answers": { QUESTION: { "type": "choice", "choice": "好难喝", "confidence": 0.8 } },
            "usage": { "input_tokens": 120, "output_tokens": 12 },
        });
        assert_eq!(parse_typesafe(&reply, &CANDIDATES), Ok(Pick { index: 1, confidence: 0.8, input_tokens: 120 }));
        assert!(parse_typesafe(&reply, &["好难和"]).is_err(), "挑了候选以外的");

        let reply = json!({ "choices": [{ "message": { "content": "2" } }], "usage": { "prompt_tokens": 90 } });
        assert_eq!(parse_openai(&reply, 2), Ok(Pick { index: 1, confidence: 1.0, input_tokens: 90 }));

        let reply = json!({
            "stop_reason": "end_turn",
            "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": "第 2 句" }],
            "usage": { "input_tokens": 80 },
        });
        assert_eq!(parse_anthropic(&reply, 2), Ok(Pick { index: 1, confidence: 1.0, input_tokens: 80 }));
        assert!(parse_anthropic(&json!({ "stop_reason": "refusal", "content": [] }), 2).is_err());
    }

    #[test]
    fn numbers_out_of_range_are_rejected() {
        assert_eq!(chosen_number("1", 3), Ok(0));
        assert_eq!(chosen_number("答案是 3。", 3), Ok(2));
        assert!(chosen_number("4", 3).is_err());
        assert!(chosen_number("0", 3).is_err());
        assert!(chosen_number("好难喝", 3).is_err());
    }

    #[test]
    fn errors_keep_only_the_reason() {
        let reply = json!({ "error": { "message": "Model Not Exist", "type": "invalid_request_error" } });
        assert_eq!(reason(&reply), "：Model Not Exist");
        assert_eq!(reason(&json!({ "detail": [{ "input": "光标前的字" }] })), "", "整个回复可能带着发过去的字");
        let error = Error::Status { status: 400, reason: "：invalid content: 周末想去夜市吃点".into() };
        assert!(error.to_string().contains("夜市"), "给人看的带着原话");
        assert_eq!(error.log_line(), "请求失败（HTTP 400）", "写日志的不带");
    }
}
