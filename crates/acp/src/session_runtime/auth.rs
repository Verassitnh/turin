//! Authentication method selection for ACP session runtime.

/// Minimal auth method shape used by the runtime (upstream `AuthMethod`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAuthMethod {
    pub id: String,
    pub method_type: Option<String>,
}

/// Port of upstream `selectAcpAgentAuthMethod`.
pub fn select_acp_agent_auth_method(
    auth_methods: Option<&[AcpAuthMethod]>,
    preferred_method_id: Option<&str>,
) -> Option<AcpAuthMethod> {
    let preferred = preferred_method_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(preferred) = preferred {
        return auth_methods
            .and_then(|methods| methods.iter().find(|method| method.id == preferred))
            .cloned();
    }
    auth_methods.and_then(|methods| {
        methods
            .iter()
            .find(|method| {
                method.method_type.is_none() || method.method_type.as_deref() == Some("agent")
            })
            .cloned()
    })
}
