use crate::agents::definition::*;
use anyhow::{Context, Result, bail};
use serde_yaml::{Mapping, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use url::Url;
const MAX_FILE_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkdownFlavor {
    Canonical,
    Claude,
    OpenCode,
}
pub(crate) fn parse_markdown(
    path: PathBuf,
    input: &str,
    flavor: MarkdownFlavor,
) -> Result<Option<AgentDefinition>> {
    if input.len() > MAX_FILE_BYTES {
        bail!("agent markdown exceeds 1 MiB")
    }
    let (frontmatter, instructions) = split_frontmatter(input)?;
    let map: Mapping =
        serde_yaml::from_str(frontmatter).context("invalid agent YAML frontmatter")?;
    if flavor == MarkdownFlavor::OpenCode && string(&map, "mode").as_deref() == Some("primary") {
        return Ok(None);
    }
    reject_provider_secrets(&map)?;
    let name = string(&map, "name")
        .or_else(|| {
            (flavor == MarkdownFlavor::OpenCode)
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .context("agent name is required")?;
    validate_name(&name)?;
    let description = required(&map, "description")?;
    if instructions.trim().is_empty() {
        bail!("agent instructions must be nonempty")
    }
    let (provider_text, model) = match flavor {
        MarkdownFlavor::OpenCode => open_code_model(&map)?,
        MarkdownFlavor::Claude => (
            string_alias(&map, &["modelProvider", "model_provider"])
                .unwrap_or_else(|| "anthropic".into()),
            required(&map, "model")?,
        ),
        MarkdownFlavor::Canonical => (
            string_alias(&map, &["modelProvider", "model_provider"])
                .context("model_provider is required for canonical agents")?,
            required(&map, "model")?,
        ),
    };
    let provider = parse_provider(&provider_text)?;
    let (base_url, env_key, wire_api) = endpoint(
        &provider,
        string_alias(&map, &["baseUrl", "base_url"]).as_deref(),
        string_alias(&map, &["envKey", "env_key"]).as_deref(),
        string_alias(&map, &["wireApi", "wire_api"]).as_deref(),
    )?;
    if flavor == MarkdownFlavor::OpenCode
        && matches!(provider, ModelProviderKind::Custom)
        && wire_api != WireApi::Responses
    {
        bail!("OpenRouter requires the responses wire API")
    }
    if flavor == MarkdownFlavor::Claude {
        diagnose_unsupported_claude_fields(&map);
    }
    let temperature = number_alias(&map, &["temperature"]);
    if let Some(v) = temperature {
        validate_temperature(v, &wire_api)?
    }
    let reasoning_effort = string_alias(&map, &["effort", "reasoningEffort", "reasoning_effort"])
        .map(|v| normalize_reasoning_effort(&v))
        .transpose()?;
    let max_turns = integer_alias(&map, &["maxTurns", "max_turns", "steps"])
        .unwrap_or(DEFAULT_MAX_TURNS as i64);
    if !(1..=MAX_TURNS as i64).contains(&max_turns) {
        bail!("max_turns must be between 1 and {MAX_TURNS}")
    }
    Ok(Some(AgentDefinition {
        name,
        description,
        instructions: instructions.into(),
        model,
        provider,
        base_url,
        env_key,
        wire_api,
        reasoning_effort,
        temperature,
        max_turns: max_turns as u32,
        permission: permission_policy(&map)?,
        sandbox: string(&map, "sandbox")
            .as_deref()
            .map(parse_sandbox)
            .transpose()?
            .unwrap_or(SandboxMode::Default),
        isolation: string(&map, "isolation")
            .as_deref()
            .map(parse_isolation)
            .transpose()?
            .unwrap_or(IsolationMode::None),
        skills: strings(&map, "skills")?,
        mcp_servers: mcp_servers(&map)?,
        source_path: path,
    }))
}
fn split_frontmatter(input: &str) -> Result<(&str, &str)> {
    let rest = input
        .strip_prefix("---\n")
        .or_else(|| input.strip_prefix("---\r\n"))
        .context("agent markdown must begin with YAML frontmatter")?;
    for marker in ["\n---\n", "\n---\r\n"] {
        if let Some(pair) = rest.split_once(marker) {
            return Ok(pair);
        }
    }
    bail!("agent YAML frontmatter is not terminated")
}
fn key(k: &str) -> Value {
    Value::String(k.into())
}
fn string(map: &Mapping, k: &str) -> Option<String> {
    map.get(key(k)).and_then(Value::as_str).map(str::to_owned)
}
fn string_alias(map: &Mapping, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| string(map, k))
}
fn required(map: &Mapping, k: &str) -> Result<String> {
    string(map, k)
        .filter(|v| !v.trim().is_empty())
        .context(format!("{k} is required"))
}
fn integer_alias(map: &Mapping, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|k| map.get(key(k)).and_then(Value::as_i64))
}
fn number_alias(map: &Mapping, keys: &[&str]) -> Option<f32> {
    keys.iter()
        .find_map(|k| map.get(key(k)).and_then(Value::as_f64).map(|v| v as f32))
}
fn strings(map: &Mapping, k: &str) -> Result<Vec<String>> {
    match map.get(key(k)) {
        None => Ok(vec![]),
        Some(Value::Sequence(v)) => v
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .context("array entries must be strings")
            })
            .collect(),
        _ => bail!("{k} must be an array"),
    }
}
fn reject_provider_secrets(map: &Mapping) -> Result<()> {
    for k in ["api_key", "apiKey", "token", "access_token", "accessToken"] {
        if map.contains_key(key(k)) {
            bail!("literal secret field {k} is prohibited")
        }
    }
    if let Some(Value::Mapping(provider)) = map.get(key("provider")) {
        for k in ["api_key", "apiKey", "token", "access_token", "accessToken"] {
            if provider.contains_key(key(k)) {
                bail!("literal provider secret field {k} is prohibited")
            }
        }
    }
    Ok(())
}
fn endpoint(
    provider: &ModelProviderKind,
    base: Option<&str>,
    env: Option<&str>,
    wire: Option<&str>,
) -> Result<(Url, String, WireApi)> {
    if matches!(provider, ModelProviderKind::Custom) {
        let url = Url::parse(base.context("custom provider requires base_url")?)?;
        validate_endpoint(&url)?;
        let key = env.context("custom provider requires env_key")?.to_owned();
        validate_env_key(&key)?;
        let api = parse_wire_api(wire.context("custom provider requires wire_api")?)?;
        return Ok((url, key, api));
    }
    let (mut url, mut key, mut api) = defaults_for(provider)?;
    if let Some(v) = base {
        url = Url::parse(v)?;
        validate_endpoint(&url)?
    }
    if let Some(v) = env {
        validate_env_key(v)?;
        key = v.into()
    }
    if let Some(v) = wire {
        api = parse_wire_api(v)?
    }
    validate_provider_wire(provider, &api)?;
    Ok((url, key, api))
}
fn open_code_model(map: &Mapping) -> Result<(String, String)> {
    let value = required(map, "model")?;
    let (prefix, model) = value
        .split_once('/')
        .context("OpenCode model must use a supported provider/model prefix")?;
    if model.is_empty() {
        bail!("OpenCode model ID is required")
    }
    match prefix {
        "openai" | "anthropic" => Ok((prefix.into(), model.into())),
        "openrouter" => Ok(("custom".into(), model.into())),
        _ => bail!("unsupported OpenCode provider {prefix:?}"),
    }
}
fn permission_policy(map: &Mapping) -> Result<PermissionPolicy> {
    let mode = string_alias(map, &["permission", "permissionMode"])
        .as_deref()
        .map(parse_permission)
        .transpose()?
        .unwrap_or(PermissionMode::Default);
    let allowed = if map.contains_key(key("tools")) {
        Some(strings(map, "tools")?.into_iter().collect::<BTreeSet<_>>())
    } else {
        None
    };
    let denied = strings_alias(map, &["disallowedTools", "disallowed_tools"])?
        .into_iter()
        .collect();
    Ok(PermissionPolicy {
        mode,
        allowed_mcp_tools: allowed,
        disallowed_mcp_tools: denied,
    })
}
fn strings_alias(map: &Mapping, keys: &[&str]) -> Result<Vec<String>> {
    for k in keys {
        if map.contains_key(key(k)) {
            return strings(map, k);
        }
    }
    Ok(vec![])
}
fn diagnose_unsupported_claude_fields(map: &Mapping) {
    const SUPPORTED: &[&str] = &[
        "name",
        "description",
        "model",
        "modelProvider",
        "model_provider",
        "baseUrl",
        "base_url",
        "envKey",
        "env_key",
        "wireApi",
        "wire_api",
        "maxTurns",
        "max_turns",
        "temperature",
        "reasoningEffort",
        "reasoning_effort",
        "effort",
        "permissionMode",
        "permission",
        "tools",
        "disallowedTools",
        "disallowed_tools",
        "skills",
        "mcpServers",
        "mcp_servers",
        "sandbox",
        "isolation",
    ];
    for key in map.keys().filter_map(Value::as_str) {
        if !SUPPORTED.contains(&key) {
            tracing::warn!(field = key, "ignoring unsupported Claude agent field");
        }
    }
}
fn mcp_servers(map: &Mapping) -> Result<BTreeMap<String, McpServerDefinition>> {
    let Some(Value::Mapping(servers)) = map
        .get(key("mcpServers"))
        .or_else(|| map.get(key("mcp_servers")))
    else {
        return Ok(BTreeMap::new());
    };
    servers
        .iter()
        .map(|(name, value)| {
            let name = name
                .as_str()
                .context("MCP server name must be a string")?
                .to_owned();
            let cfg = value.as_mapping().context("MCP server must be a map")?;
            let server = match string(cfg, "type").as_deref() {
                Some("sse") => bail!("MCP server type sse is deprecated"),
                Some("http") => McpServerDefinition::Http {
                    url: checked_url(&required(cfg, "url")?)?,
                    headers: string_map(cfg, "headers")?,
                },
                Some("stdio") | None if cfg.contains_key(key("command")) => {
                    McpServerDefinition::Stdio {
                        command: required(cfg, "command")?,
                        args: strings(cfg, "args")?,
                        env: string_map(cfg, "env")?,
                    }
                }
                _ => bail!("unsupported MCP server type"),
            };
            Ok((name, server))
        })
        .collect()
}
fn checked_url(s: &str) -> Result<Url> {
    let u = Url::parse(s)?;
    validate_endpoint(&u)?;
    Ok(u)
}
fn string_map(map: &Mapping, k: &str) -> Result<BTreeMap<String, String>> {
    match map.get(key(k)) {
        None => Ok(BTreeMap::new()),
        Some(Value::Mapping(v)) => v
            .iter()
            .map(|(a, b)| {
                Ok((
                    a.as_str().context("map key must be a string")?.into(),
                    b.as_str().context("map value must be a string")?.into(),
                ))
            })
            .collect(),
        _ => bail!("{k} must be a map"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_defaults_and_normalizes_policy() {
        let input = "---\nname: claude\ndescription: x\nmodel: claude-sonnet\npermissionMode: plan\ntools: [child/a]\ndisallowedTools: [child/b]\nreasoningEffort: HIGH\ntemperature: 1\n---\nbody";
        let agent = parse_markdown(PathBuf::from("a.md"), input, MarkdownFlavor::Claude)
            .unwrap()
            .unwrap();
        assert!(matches!(agent.provider, ModelProviderKind::Anthropic));
        assert_eq!(agent.reasoning_effort.as_deref(), Some("high"));
        assert!(agent.permission.allowed_mcp_tools.is_some());
    }
    #[test]
    fn canonical_requires_provider_and_mcp_stdio_args_are_optional() {
        let missing = "---\nname: a\ndescription: x\nmodel: g\n---\nbody";
        assert!(parse_markdown(PathBuf::from("a.md"), missing, MarkdownFlavor::Canonical).is_err());
        let input = "---\nname: a\ndescription: x\nmodel: g\nmodelProvider: openai\nmcpServers: { child: { type: stdio, command: node, env: { GITHUB_TOKEN: '${GITHUB_TOKEN}' } } }\n---\nbody";
        let agent = parse_markdown(PathBuf::from("a.md"), input, MarkdownFlavor::Canonical)
            .unwrap()
            .unwrap();
        assert!(
            matches!(&agent.mcp_servers["child"], McpServerDefinition::Stdio { args, .. } if args.is_empty())
        );
    }
    #[test]
    fn opencode_openrouter_needs_responses_configuration() {
        let input = "---\ndescription: x\nmodel: openrouter/vendor/model\nbase_url: https://router.test\nenv_key: ROUTER_KEY\nwire_api: anthropic-messages\n---\nbody";
        assert!(parse_markdown(PathBuf::from("a.md"), input, MarkdownFlavor::OpenCode).is_err());
    }

    #[test]
    fn markdown_security_modes_fail_closed_for_canonical_and_vendor_files() {
        for flavor in [
            MarkdownFlavor::Canonical,
            MarkdownFlavor::Claude,
            MarkdownFlavor::OpenCode,
        ] {
            let provider = if flavor == MarkdownFlavor::Canonical {
                "model_provider: openai\n"
            } else {
                ""
            };
            let name = if flavor == MarkdownFlavor::OpenCode {
                ""
            } else {
                "name: secure\n"
            };
            for field in ["isolation: container", "sandbox: workspace-write"] {
                let input =
                    format!("---\n{name}description: x\nmodel: g\n{provider}{field}\n---\nbody");
                assert!(
                    parse_markdown(PathBuf::from("secure.md"), &input, flavor).is_err(),
                    "{field} was accepted"
                );
            }
        }
    }
}
