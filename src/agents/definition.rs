use anyhow::{Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use url::{Host, Url};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum ModelProviderKind {
    OpenAi,
    Anthropic,
    Custom,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum WireApi {
    Responses,
    AnthropicMessages,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum PermissionMode {
    Default,
    ReadOnly,
}
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct PermissionPolicy {
    pub(crate) mode: PermissionMode,
    /// If present, only these child MCP tools may be exposed.
    pub(crate) allowed_mcp_tools: Option<BTreeSet<String>>,
    pub(crate) disallowed_mcp_tools: BTreeSet<String>,
}
impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Default,
            allowed_mcp_tools: None,
            disallowed_mcp_tools: BTreeSet::new(),
        }
    }
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum SandboxMode {
    Default,
    ReadOnly,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum IsolationMode {
    None,
}
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum McpServerDefinition {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: Url,
        headers: BTreeMap<String, String>,
    },
}
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct AgentDefinition {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) instructions: String,
    pub(crate) model: String,
    pub(crate) provider: ModelProviderKind,
    pub(crate) base_url: Url,
    pub(crate) env_key: String,
    pub(crate) wire_api: WireApi,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) temperature: Option<f32>,
    pub(crate) max_turns: u32,
    pub(crate) permission: PermissionPolicy,
    pub(crate) sandbox: SandboxMode,
    pub(crate) isolation: IsolationMode,
    pub(crate) skills: Vec<String>,
    pub(crate) mcp_servers: BTreeMap<String, McpServerDefinition>,
    pub(crate) source_path: PathBuf,
}
pub(crate) const DEFAULT_MAX_TURNS: u32 = 32;
pub(crate) const MAX_TURNS: u32 = 1_000;
pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("agent name must be 1–64 characters")
    }
    let mut separator = false;
    for (i, byte) in name.bytes().enumerate() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            separator = false;
        } else if (byte == b'_' || byte == b'-') && i != 0 && !separator {
            separator = true;
        } else {
            bail!("invalid agent name {name:?}")
        }
    }
    if separator {
        bail!("invalid agent name {name:?}")
    }
    Ok(())
}
pub(crate) fn validate_endpoint(url: &Url) -> Result<()> {
    if url.scheme() == "https" {
        return Ok(());
    }
    let loopback = matches!(url.host(), Some(Host::Domain(h)) if h.eq_ignore_ascii_case("localhost"))
        || matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_loopback());
    if url.scheme() == "http" && loopback {
        Ok(())
    } else {
        bail!("endpoint must use HTTPS (HTTP is allowed only for loopback)")
    }
}
pub(crate) fn parse_provider(value: &str) -> Result<ModelProviderKind> {
    match value {
        "openai" => Ok(ModelProviderKind::OpenAi),
        "anthropic" => Ok(ModelProviderKind::Anthropic),
        "custom" => Ok(ModelProviderKind::Custom),
        _ => bail!("unsupported model provider {value:?}"),
    }
}
pub(crate) fn parse_wire_api(value: &str) -> Result<WireApi> {
    match value {
        "responses" => Ok(WireApi::Responses),
        "anthropic-messages" => Ok(WireApi::AnthropicMessages),
        _ => bail!("unsupported wire API {value:?}"),
    }
}
pub(crate) fn defaults_for(provider: &ModelProviderKind) -> Result<(Url, String, WireApi)> {
    match provider {
        ModelProviderKind::OpenAi => Ok((
            Url::parse("https://api.openai.com/v1")?,
            "OPENAI_API_KEY".into(),
            WireApi::Responses,
        )),
        ModelProviderKind::Anthropic => Ok((
            Url::parse("https://api.anthropic.com")?,
            "ANTHROPIC_API_KEY".into(),
            WireApi::AnthropicMessages,
        )),
        ModelProviderKind::Custom => {
            bail!("custom provider requires explicit base_url, env_key, and wire_api")
        }
    }
}
pub(crate) fn validate_provider_wire(provider: &ModelProviderKind, wire: &WireApi) -> Result<()> {
    match (provider, wire) {
        (ModelProviderKind::OpenAi, WireApi::Responses)
        | (ModelProviderKind::Anthropic, WireApi::AnthropicMessages)
        | (ModelProviderKind::Custom, _) => Ok(()),
        _ => bail!("provider and wire_api are inconsistent"),
    }
}
pub(crate) fn validate_env_key(value: &str) -> Result<()> {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        bail!("env_key must be a nonempty environment variable identifier")
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        bail!("env_key must be a nonempty environment variable identifier")
    }
    Ok(())
}
pub(crate) fn normalize_reasoning_effort(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" => Ok(value),
        _ => bail!("invalid reasoning effort"),
    }
}
pub(crate) fn validate_temperature(value: f32, wire: &WireApi) -> Result<()> {
    if !value.is_finite() {
        bail!("temperature must be finite")
    }
    let max = if matches!(wire, WireApi::Responses) {
        2.0
    } else {
        1.0
    };
    if !(0.0..=max).contains(&value) {
        bail!("temperature is outside the range supported by the selected wire API")
    }
    Ok(())
}
pub(crate) fn parse_permission(value: &str) -> Result<PermissionMode> {
    match value {
        "default" | "acceptEdits" => Ok(PermissionMode::Default),
        "read-only" | "readonly" | "plan" => Ok(PermissionMode::ReadOnly),
        "full-access" | "full_access" | "bypassPermissions" | "dontAsk" => {
            bail!("unsupported permission mode {value:?}")
        }
        _ => bail!("invalid permission mode {value:?}"),
    }
}
pub(crate) fn parse_sandbox(value: &str) -> Result<SandboxMode> {
    match value {
        "default" => Ok(SandboxMode::Default),
        "read-only" | "readonly" => Ok(SandboxMode::ReadOnly),
        "workspace-write" | "workspace_write" | "danger-full-access" | "danger_full_access" => {
            bail!("unsupported sandbox mode {value:?}")
        }
        _ => bail!("invalid sandbox mode {value:?}"),
    }
}
pub(crate) fn parse_isolation(value: &str) -> Result<IsolationMode> {
    match value {
        "none" => Ok(IsolationMode::None),
        "worktree" | "container" => bail!("unsupported isolation mode {value:?}"),
        _ => bail!("invalid isolation mode {value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_environment_reasoning_and_adapter_ranges() {
        assert!(validate_env_key("GITHUB_TOKEN").is_ok());
        assert!(validate_env_key("1TOKEN").is_err());
        assert_eq!(normalize_reasoning_effort(" XHIGH ").unwrap(), "xhigh");
        assert!(validate_temperature(1.1, &WireApi::AnthropicMessages).is_err());
        assert!(parse_isolation("vm").is_err());
    }

    #[test]
    fn rejects_unenforced_security_modes() {
        assert_eq!(parse_isolation("none").unwrap(), IsolationMode::None);
        for value in ["worktree", "container"] {
            assert!(
                parse_isolation(value)
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported isolation")
            );
        }
        assert_eq!(parse_sandbox("default").unwrap(), SandboxMode::Default);
        assert_eq!(parse_sandbox("read-only").unwrap(), SandboxMode::ReadOnly);
        for value in ["workspace-write", "danger-full-access"] {
            assert!(
                parse_sandbox(value)
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported sandbox")
            );
        }
        assert_eq!(
            parse_permission("default").unwrap(),
            PermissionMode::Default
        );
        assert_eq!(
            parse_permission("read-only").unwrap(),
            PermissionMode::ReadOnly
        );
        for value in ["full-access", "full_access", "bypassPermissions", "dontAsk"] {
            assert!(
                parse_permission(value)
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported permission")
            );
        }
    }
}
