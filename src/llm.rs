use crate::dwarf_data::DwarfData;
use std::fs;
use std::path::Path;

/// LLM 返回的断点解析结果
#[derive(Debug)]
pub enum BreakpointSpec {
    /// 按行号设置断点（可选文件名）
    Line { file: Option<String>, line: usize },
    /// 按函数名设置断点
    Function { name: String },
    /// 按地址设置断点
    Address { addr: usize },
}

/// LLM API 配置
struct LlmConfig {
    api_key: String,
    api_base: String,
    model: String,
}

/// 从配置文件加载 LLM 配置
/// 查找顺序: ./llm_config.json -> ~/.deet_llm_config.json
fn load_config() -> Result<LlmConfig, String> {
    let config_paths = vec![
        "llm_config.json".to_string(),
        format!(
            "{}/.deet_llm_config.json",
            std::env::var("HOME").unwrap_or_default()
        ),
    ];

    let mut config_content = None;
    let mut used_path = String::new();
    for path in &config_paths {
        if Path::new(path).exists() {
            match fs::read_to_string(path) {
                Ok(content) => {
                    config_content = Some(content);
                    used_path = path.clone();
                    break;
                }
                Err(e) => {
                    return Err(format!("读取配置文件 {} 失败: {}", path, e));
                }
            }
        }
    }

    let content = config_content.ok_or_else(|| {
        "未找到 LLM 配置文件。请创建以下任一文件:\n\
         - ./llm_config.json\n\
         - ~/.deet_llm_config.json\n\
         \n\
         文件内容示例:\n\
         {\n\
         \x20   \"api_key\": \"your-api-key\",\n\
         \x20   \"api_base\": \"https://api.openai.com/v1\",\n\
         \x20   \"model\": \"gpt-4o-mini\"\n\
         }"
        .to_string()
    })?;

    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("解析配置文件 {} 失败: {}", used_path, e))?;

    let api_key = json["api_key"]
        .as_str()
        .ok_or_else(|| "配置文件缺少 api_key 字段".to_string())?
        .to_string();

    if api_key == "your-api-key-here" || api_key.is_empty() {
        return Err("请在配置文件中填入有效的 api_key".to_string());
    }

    let api_base = json["api_base"]
        .as_str()
        .unwrap_or("https://api.openai.com/v1")
        .to_string();

    let model = json["model"].as_str().unwrap_or("gpt-4o-mini").to_string();

    Ok(LlmConfig {
        api_key,
        api_base,
        model,
    })
}

/// 从 DWARF 数据中收集调试上下文，作为 LLM 的 system prompt 上下文
fn build_debug_context(_debug_data: &DwarfData) -> String {
    String::from(
        "可用的断点类型：\n\
         1. 函数名断点：指定函数名\n\
         2. 行号断点：指定行号（可选文件名）\n\
         3. 地址断点：指定十六进制地址",
    )
}

/// 调用 LLM API 将自然语言转换为断点规格
pub fn parse_natural_breakpoint(
    natural_text: &str,
    debug_data: &DwarfData,
) -> Result<BreakpointSpec, String> {
    let config = load_config()?;

    let debug_context = build_debug_context(debug_data);

    let system_prompt = format!(
        r#"你是一个调试器断点解析助手。用户会用自然语言描述想要设置断点的位置，你需要将其解析为结构化的 JSON 格式。

当前调试程序的信息：
{debug_context}

你必须返回且只返回一个 JSON 对象（不要包含任何其他文字），格式为以下三种之一：

1. 按行号设置断点：
   {{"type": "line", "file": "文件名或null", "line": 行号数字}}

2. 按函数名设置断点：
   {{"type": "function", "name": "函数名"}}

3. 按地址设置断点：
   {{"type": "address", "addr": "0x十六进制地址"}}

注意：
- file 字段可以为 null（如果用户没指定文件）
- line 必须是正整数
- name 是 C/C++ 函数名（如 main, func1 等）
- addr 是以 0x 开头的十六进制字符串

示例：
用户："在main函数设断点" -> {{"type": "function", "name": "main"}}
用户："第5行断点" -> {{"type": "line", "file": null, "line": 5}}
用户："在count.c的第10行停下来" -> {{"type": "line", "file": "count.c", "line": 10}}
用户："在地址0x4005b8设断点" -> {{"type": "address", "addr": "0x4005b8"}}"#
    );

    let request_body = serde_json::json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": natural_text}
        ],
        "temperature": 0.0,
        "max_tokens": 150
    });

    let url = format!("{}/chat/completions", config.api_base.trim_end_matches('/'));

    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", config.api_key))
        .set("Content-Type", "application/json")
        .send_string(&request_body.to_string())
        .map_err(|e| format!("LLM API 请求失败: {}", e))?;

    let response_text = response
        .into_string()
        .map_err(|e| format!("读取 LLM 响应失败: {}", e))?;

    let response_json: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("解析 LLM 响应 JSON 失败: {}", e))?;

    // 提取 LLM 返回的内容
    let content = response_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("LLM 响应格式异常: {}", response_text))?;

    // 尝试从内容中提取 JSON（LLM 可能会用 ```json ``` 包裹）
    let json_str = extract_json(content);

    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("解析 LLM 返回的断点 JSON 失败: {} (原文: {})", e, content))?;

    // 转换为 BreakpointSpec
    match parsed["type"].as_str() {
        Some("line") => {
            let line = parsed["line"]
                .as_u64()
                .ok_or_else(|| "LLM 返回的行号无效".to_string())? as usize;
            let file = parsed["file"].as_str().map(|s| s.to_string());
            Ok(BreakpointSpec::Line { file, line })
        }
        Some("function") => {
            let name = parsed["name"]
                .as_str()
                .ok_or_else(|| "LLM 返回的函数名无效".to_string())?
                .to_string();
            Ok(BreakpointSpec::Function { name })
        }
        Some("address") => {
            let addr_str = parsed["addr"]
                .as_str()
                .ok_or_else(|| "LLM 返回的地址无效".to_string())?;
            let addr_hex = addr_str.trim_start_matches("0x").trim_start_matches("0X");
            let addr =
                usize::from_str_radix(addr_hex, 16).map_err(|e| format!("解析地址失败: {}", e))?;
            Ok(BreakpointSpec::Address { addr })
        }
        other => Err(format!(
            "LLM 返回了未知的断点类型: {:?} (原文: {})",
            other, content
        )),
    }
}

/// 从 LLM 的回答中提取 JSON 字符串（处理可能的 markdown 代码块包裹）
fn extract_json(content: &str) -> String {
    let trimmed = content.trim();

    // 尝试提取 ```json ... ``` 格式
    if let Some(start) = trimmed.find("```json") {
        let after_marker = &trimmed[start + 7..];
        if let Some(end) = after_marker.find("```") {
            return after_marker[..end].trim().to_string();
        }
    }

    // 尝试提取 ``` ... ``` 格式
    if let Some(start) = trimmed.find("```") {
        let after_marker = &trimmed[start + 3..];
        if let Some(end) = after_marker.find("```") {
            return after_marker[..end].trim().to_string();
        }
    }

    // 尝试提取 { ... } 格式
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    }

    trimmed.to_string()
}
