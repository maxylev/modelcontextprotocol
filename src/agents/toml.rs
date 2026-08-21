use crate::agents::definition::*;
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, path::PathBuf};
use toml::Table;
use url::Url;
const MAX_FILE_BYTES: usize = 1024 * 1024;
pub(crate) fn parse_toml(path: PathBuf, input: &str) -> Result<AgentDefinition> {
    if input.len() > MAX_FILE_BYTES {
        bail!("agent TOML exceeds 1 MiB")
    }
    let t: Table = toml::from_str(input).context("invalid agent TOML")?;
    reject_provider_secrets(&t)?;
    let name = required(&t, "name")?;
    validate_name(&name)?;
    let description = required(&t, "description")?;
    let instructions = string(&t, "instructions")
        .or_else(|| string(&t, "developer_instructions"))
        .filter(|v| !v.trim().is_empty())
        .context("instructions is required")?;
    let model = required(&t, "model")?;
    let provider = parse_provider(
        &string_alias(&t, &["model_provider", "modelProvider"]).unwrap_or_else(|| "openai".into()),
    )?;
    let (base_url, env_key, wire_api) = endpoint(
        &provider,
        string_alias(&t, &["base_url", "baseUrl"]).as_deref(),
        string_alias(&t, &["env_key", "envKey"]).as_deref(),
        string_alias(&t, &["wire_api", "wireApi"]).as_deref(),
    )?;
    let temperature = t
        .get("temperature")
        .and_then(|v| v.as_float())
        .map(|v| v as f32);
    if let Some(v) = temperature {
        validate_temperature(v, &wire_api)?
    }
    let reasoning_effort = string_alias(&t, &["model_reasoning_effort", "reasoning_effort"])
        .map(|v| normalize_reasoning_effort(&v))
        .transpose()?;
    let turns =
        integer_alias(&t, &["max_turns", "maxTurns", "steps"]).unwrap_or(DEFAULT_MAX_TURNS as i64);
    if !(1..=MAX_TURNS as i64).contains(&turns) {
        bail!("max_turns must be between 1 and {MAX_TURNS}")
    }
    Ok(AgentDefinition {
        name,
        description,
        instructions,
        model,
        provider,
        base_url,
        env_key,
        wire_api,
        reasoning_effort,
        temperature,
        max_turns: turns as u32,
        permission: PermissionPolicy {
            mode: string_alias(&t, &["permission", "permission_mode"])
                .as_deref()
                .map(parse_permission)
                .transpose()?
                .unwrap_or(PermissionMode::Default),
            allowed_mcp_tools: None,
            disallowed_mcp_tools: Default::default(),
        },
        sandbox: string(&t, "sandbox_mode")
            .as_deref()
            .map(parse_sandbox)
            .transpose()?
            .unwrap_or(SandboxMode::Default),
        isolation: string(&t, "isolation")
            .as_deref()
            .map(parse_isolation)
            .transpose()?
            .unwrap_or(IsolationMode::None),
        skills: strings(&t, "skills")?,
        mcp_servers: mcp_servers(&t)?,
        source_path: path,
    })
}
fn string(t: &Table, k: &str) -> Option<String> {
    t.get(k).and_then(|v| v.as_str()).map(str::to_owned)
}
fn string_alias(t: &Table, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| string(t, k))
}
fn required(t: &Table, k: &str) -> Result<String> {
    string(t, k)
        .filter(|v| !v.trim().is_empty())
        .context(format!("{k} is required"))
}
fn integer_alias(t: &Table, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|k| t.get(*k).and_then(|v| v.as_integer()))
}
fn strings(t: &Table, k: &str) -> Result<Vec<String>> {
    match t.get(k) {
        None => Ok(vec![]),
        Some(v) => v
            .as_array()
            .context(format!("{k} must be an array"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .context("array entries must be strings")
            })
            .collect(),
    }
}
fn reject_provider_secrets(t: &Table) -> Result<()> {
    for k in ["api_key", "apiKey", "token", "access_token", "accessToken"] {
        if t.contains_key(k) {
            bail!("literal secret field {k} is prohibited")
        }
    }
    if let Some(provider) = t.get("provider").and_then(|v| v.as_table()) {
        for k in ["api_key", "apiKey", "token", "access_token", "accessToken"] {
            if provider.contains_key(k) {
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
        return Ok((
            url,
            key,
            parse_wire_api(wire.context("custom provider requires wire_api")?)?,
        ));
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
fn mcp_servers(t: &Table) -> Result<BTreeMap<String, McpServerDefinition>> {
    let Some(servers) = t.get("mcp_servers").or_else(|| t.get("mcpServers")) else {
        return Ok(BTreeMap::new());
    };
    servers
        .as_table()
        .context("mcp_servers must be a table")?
        .iter()
        .map(|(name, value)| {
            let c = value.as_table().context("MCP server must be a table")?;
            let server = match string(c, "type").as_deref() {
                Some("sse") => bail!("MCP server type sse is deprecated"),
                Some("http") => McpServerDefinition::Http {
                    url: checked_url(&required(c, "url")?)?,
                    headers: string_map(c, "headers")?,
                },
                Some("stdio") | None if c.contains_key("command") => McpServerDefinition::Stdio {
                    command: required(c, "command")?,
                    args: strings(c, "args")?,
                    env: string_map(c, "env")?,
                },
                _ => bail!("unsupported MCP server type"),
            };
            Ok((name.clone(), server))
        })
        .collect()
}
fn checked_url(s: &str) -> Result<Url> {
    let u = Url::parse(s)?;
    validate_endpoint(&u)?;
    Ok(u)
}
fn string_map(t: &Table, k: &str) -> Result<BTreeMap<String, String>> {
    match t.get(k) {
        None => Ok(BTreeMap::new()),
        Some(v) => v
            .as_table()
            .context(format!("{k} must be a table"))?
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    v.as_str().context("map values must be strings")?.into(),
                ))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_reasoning_temperature_and_sse_validation() {
        let valid = "name='a'\ndescription='x'\ninstructions='body'\nmodel='gpt'\nmodel_reasoning_effort='medium'\ntemperature=2.0\n[mcp_servers.local]\ntype='stdio'\ncommand='node'";
        let agent = parse_toml(PathBuf::from("a.toml"), valid).unwrap();
        assert_eq!(agent.reasoning_effort.as_deref(), Some("medium"));
        let invalid = "name='a'\ndescription='x'\ninstructions='body'\nmodel='gpt'\n[mcp_servers.old]\ntype='sse'\nurl='https://example.test'";
        assert!(parse_toml(PathBuf::from("a.toml"), invalid).is_err());
    }
    #[test]
    fn provider_wire_conflicts_are_rejected() {
        let input = "name='a'\ndescription='x'\ninstructions='body'\nmodel='gpt'\nwire_api='anthropic-messages'";
        assert!(parse_toml(PathBuf::from("a.toml"), input).is_err());
    }

    #[test]
    fn toml_security_modes_fail_closed() {
        let base = "name='a'\ndescription='x'\ninstructions='body'\nmodel='gpt'\n";
        assert!(parse_toml(PathBuf::from("a.toml"), &format!("{base}isolation='none'")).is_ok());
        for field in [
            "isolation='worktree'",
            "isolation='container'",
            "sandbox_mode='workspace-write'",
            "sandbox_mode='danger-full-access'",
        ] {
            assert!(parse_toml(PathBuf::from("a.toml"), &format!("{base}{field}")).is_err());
        }
    }
}
