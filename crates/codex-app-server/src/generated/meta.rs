//! Generated from pinned Codex schema/type declarations. Do not edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodMeta {
    pub method: &'static str,
    pub params: Option<&'static str>,
    pub response: Option<&'static str>,
}
pub const CLIENT_REQUEST_METHODS: &[MethodMeta] = &[
    MethodMeta {
        method: "initialize",
        params: Some("V1InitializeParams"),
        response: Some("V1InitializeResponse"),
    },
    MethodMeta {
        method: "thread/start",
        params: Some("V2ThreadStartParams"),
        response: Some("V2ThreadStartResponse"),
    },
    MethodMeta {
        method: "thread/resume",
        params: Some("V2ThreadResumeParams"),
        response: Some("V2ThreadResumeResponse"),
    },
    MethodMeta {
        method: "thread/fork",
        params: Some("V2ThreadForkParams"),
        response: Some("V2ThreadForkResponse"),
    },
    MethodMeta {
        method: "thread/archive",
        params: Some("V2ThreadArchiveParams"),
        response: Some("V2ThreadArchiveResponse"),
    },
    MethodMeta {
        method: "thread/delete",
        params: Some("V2ThreadDeleteParams"),
        response: Some("V2ThreadDeleteResponse"),
    },
    MethodMeta {
        method: "thread/unsubscribe",
        params: Some("V2ThreadUnsubscribeParams"),
        response: Some("V2ThreadUnsubscribeResponse"),
    },
    MethodMeta {
        method: "thread/name/set",
        params: Some("V2ThreadSetNameParams"),
        response: Some("V2ThreadSetNameResponse"),
    },
    MethodMeta {
        method: "thread/goal/set",
        params: Some("V2ThreadGoalSetParams"),
        response: Some("V2ThreadGoalSetResponse"),
    },
    MethodMeta {
        method: "thread/goal/get",
        params: Some("V2ThreadGoalGetParams"),
        response: Some("V2ThreadGoalGetResponse"),
    },
    MethodMeta {
        method: "thread/goal/clear",
        params: Some("V2ThreadGoalClearParams"),
        response: Some("V2ThreadGoalClearResponse"),
    },
    MethodMeta {
        method: "thread/metadata/update",
        params: Some("V2ThreadMetadataUpdateParams"),
        response: Some("V2ThreadMetadataUpdateResponse"),
    },
    MethodMeta {
        method: "thread/attachment/add",
        params: Some("V2ThreadAttachmentAddParams"),
        response: Some("V2ThreadAttachmentAddResponse"),
    },
    MethodMeta {
        method: "thread/attachment/list",
        params: Some("V2ThreadAttachmentListParams"),
        response: Some("V2ThreadAttachmentListResponse"),
    },
    MethodMeta {
        method: "thread/attachment/remove",
        params: Some("V2ThreadAttachmentRemoveParams"),
        response: Some("V2ThreadAttachmentRemoveResponse"),
    },
    MethodMeta {
        method: "thread/section/move",
        params: Some("V2ThreadSectionMoveParams"),
        response: Some("V2ThreadSectionMoveResponse"),
    },
    MethodMeta {
        method: "thread/unarchive",
        params: Some("V2ThreadUnarchiveParams"),
        response: Some("V2ThreadUnarchiveResponse"),
    },
    MethodMeta {
        method: "thread/compact/start",
        params: Some("V2ThreadCompactStartParams"),
        response: Some("V2ThreadCompactStartResponse"),
    },
    MethodMeta {
        method: "thread/shellCommand",
        params: Some("V2ThreadShellCommandParams"),
        response: Some("V2ThreadShellCommandResponse"),
    },
    MethodMeta {
        method: "thread/approveGuardianDeniedAction",
        params: Some("V2ThreadApproveGuardianDeniedActionParams"),
        response: Some("V2ThreadApproveGuardianDeniedActionResponse"),
    },
    MethodMeta {
        method: "thread/revert",
        params: Some("V2ThreadRevertParams"),
        response: Some("V2ThreadRevertResponse"),
    },
    MethodMeta {
        method: "thread/list",
        params: Some("V2ThreadListParams"),
        response: Some("V2ThreadListResponse"),
    },
    MethodMeta {
        method: "threadSection/list",
        params: Some("V2ThreadSectionListParams"),
        response: Some("V2ThreadSectionListResponse"),
    },
    MethodMeta {
        method: "threadSection/create",
        params: Some("V2ThreadSectionCreateParams"),
        response: Some("V2ThreadSectionCreateResponse"),
    },
    MethodMeta {
        method: "threadSection/update",
        params: Some("V2ThreadSectionUpdateParams"),
        response: Some("V2ThreadSectionUpdateResponse"),
    },
    MethodMeta {
        method: "threadSection/delete",
        params: Some("V2ThreadSectionDeleteParams"),
        response: Some("V2ThreadSectionDeleteResponse"),
    },
    MethodMeta {
        method: "thread/loaded/list",
        params: Some("V2ThreadLoadedListParams"),
        response: Some("V2ThreadLoadedListResponse"),
    },
    MethodMeta {
        method: "thread/read",
        params: Some("V2ThreadReadParams"),
        response: Some("V2ThreadReadResponse"),
    },
    MethodMeta {
        method: "thread/turns/list",
        params: Some("V2ThreadTurnsListParams"),
        response: Some("V2ThreadTurnsListResponse"),
    },
    MethodMeta {
        method: "thread/items/list",
        params: Some("V2ThreadItemsListParams"),
        response: Some("V2ThreadItemsListResponse"),
    },
    MethodMeta {
        method: "thread/inject_items",
        params: Some("V2ThreadInjectItemsParams"),
        response: Some("V2ThreadInjectItemsResponse"),
    },
    MethodMeta {
        method: "skills/list",
        params: Some("V2SkillsListParams"),
        response: Some("V2SkillsListResponse"),
    },
    MethodMeta {
        method: "skills/extraRoots/set",
        params: Some("V2SkillsExtraRootsSetParams"),
        response: Some("V2SkillsExtraRootsSetResponse"),
    },
    MethodMeta {
        method: "hooks/list",
        params: Some("V2HooksListParams"),
        response: Some("V2HooksListResponse"),
    },
    MethodMeta {
        method: "marketplace/add",
        params: Some("V2MarketplaceAddParams"),
        response: Some("V2MarketplaceAddResponse"),
    },
    MethodMeta {
        method: "marketplace/remove",
        params: Some("V2MarketplaceRemoveParams"),
        response: Some("V2MarketplaceRemoveResponse"),
    },
    MethodMeta {
        method: "marketplace/upgrade",
        params: Some("V2MarketplaceUpgradeParams"),
        response: Some("V2MarketplaceUpgradeResponse"),
    },
    MethodMeta {
        method: "plugin/list",
        params: Some("V2PluginListParams"),
        response: Some("V2PluginListResponse"),
    },
    MethodMeta {
        method: "plugin/installed",
        params: Some("V2PluginInstalledParams"),
        response: Some("V2PluginInstalledResponse"),
    },
    MethodMeta {
        method: "plugin/reconcile",
        params: Some("V2PluginReconcileParams"),
        response: Some("V2PluginReconcileResponse"),
    },
    MethodMeta {
        method: "plugin/read",
        params: Some("V2PluginReadParams"),
        response: Some("V2PluginReadResponse"),
    },
    MethodMeta {
        method: "plugin/skill/read",
        params: Some("V2PluginSkillReadParams"),
        response: Some("V2PluginSkillReadResponse"),
    },
    MethodMeta {
        method: "plugin/share/save",
        params: Some("V2PluginShareSaveParams"),
        response: Some("V2PluginShareSaveResponse"),
    },
    MethodMeta {
        method: "plugin/share/updateTargets",
        params: Some("V2PluginShareUpdateTargetsParams"),
        response: Some("V2PluginShareUpdateTargetsResponse"),
    },
    MethodMeta {
        method: "plugin/share/list",
        params: Some("V2PluginShareListParams"),
        response: Some("V2PluginShareListResponse"),
    },
    MethodMeta {
        method: "plugin/share/checkout",
        params: Some("V2PluginShareCheckoutParams"),
        response: Some("V2PluginShareCheckoutResponse"),
    },
    MethodMeta {
        method: "plugin/share/delete",
        params: Some("V2PluginShareDeleteParams"),
        response: Some("V2PluginShareDeleteResponse"),
    },
    MethodMeta {
        method: "app/read",
        params: Some("V2AppsReadParams"),
        response: Some("V2AppsReadResponse"),
    },
    MethodMeta {
        method: "app/list",
        params: Some("V2AppsListParams"),
        response: Some("V2AppsListResponse"),
    },
    MethodMeta {
        method: "app/installed",
        params: Some("V2AppsInstalledParams"),
        response: Some("V2AppsInstalledResponse"),
    },
    MethodMeta {
        method: "fs/readFile",
        params: Some("V2FsReadFileParams"),
        response: Some("V2FsReadFileResponse"),
    },
    MethodMeta {
        method: "fs/writeFile",
        params: Some("V2FsWriteFileParams"),
        response: Some("V2FsWriteFileResponse"),
    },
    MethodMeta {
        method: "fs/createDirectory",
        params: Some("V2FsCreateDirectoryParams"),
        response: Some("V2FsCreateDirectoryResponse"),
    },
    MethodMeta {
        method: "fs/getMetadata",
        params: Some("V2FsGetMetadataParams"),
        response: Some("V2FsGetMetadataResponse"),
    },
    MethodMeta {
        method: "fs/readDirectory",
        params: Some("V2FsReadDirectoryParams"),
        response: Some("V2FsReadDirectoryResponse"),
    },
    MethodMeta {
        method: "fs/remove",
        params: Some("V2FsRemoveParams"),
        response: Some("V2FsRemoveResponse"),
    },
    MethodMeta {
        method: "fs/copy",
        params: Some("V2FsCopyParams"),
        response: Some("V2FsCopyResponse"),
    },
    MethodMeta {
        method: "fs/watch",
        params: Some("V2FsWatchParams"),
        response: Some("V2FsWatchResponse"),
    },
    MethodMeta {
        method: "fs/unwatch",
        params: Some("V2FsUnwatchParams"),
        response: Some("V2FsUnwatchResponse"),
    },
    MethodMeta {
        method: "skills/config/write",
        params: Some("V2SkillsConfigWriteParams"),
        response: Some("V2SkillsConfigWriteResponse"),
    },
    MethodMeta {
        method: "plugin/install",
        params: Some("V2PluginInstallParams"),
        response: Some("V2PluginInstallResponse"),
    },
    MethodMeta {
        method: "plugin/uninstall",
        params: Some("V2PluginUninstallParams"),
        response: Some("V2PluginUninstallResponse"),
    },
    MethodMeta {
        method: "turn/start",
        params: Some("V2TurnStartParams"),
        response: Some("V2TurnStartResponse"),
    },
    MethodMeta {
        method: "turn/steer",
        params: Some("V2TurnSteerParams"),
        response: Some("V2TurnSteerResponse"),
    },
    MethodMeta {
        method: "turn/interrupt",
        params: Some("V2TurnInterruptParams"),
        response: Some("V2TurnInterruptResponse"),
    },
    MethodMeta {
        method: "review/start",
        params: Some("V2ReviewStartParams"),
        response: Some("V2ReviewStartResponse"),
    },
    MethodMeta {
        method: "model/list",
        params: Some("V2ModelListParams"),
        response: Some("V2ModelListResponse"),
    },
    MethodMeta {
        method: "account/gatewayOAuth/read",
        params: None,
        response: Some("V2GatewayOAuthReadResponse"),
    },
    MethodMeta {
        method: "account/gatewayOAuth/login",
        params: None,
        response: Some("V2GatewayOAuthLoginResponse"),
    },
    MethodMeta {
        method: "account/gatewayOAuth/cancel",
        params: None,
        response: Some("V2GatewayOAuthCancelResponse"),
    },
    MethodMeta {
        method: "modelProvider/capabilities/read",
        params: Some("V2ModelProviderCapabilitiesReadParams"),
        response: Some("V2ModelProviderCapabilitiesReadResponse"),
    },
    MethodMeta {
        method: "experimentalFeature/list",
        params: Some("V2ExperimentalFeatureListParams"),
        response: Some("V2ExperimentalFeatureListResponse"),
    },
    MethodMeta {
        method: "permissionProfile/list",
        params: Some("V2PermissionProfileListParams"),
        response: Some("V2PermissionProfileListResponse"),
    },
    MethodMeta {
        method: "experimentalFeature/enablement/set",
        params: Some("V2ExperimentalFeatureEnablementSetParams"),
        response: Some("V2ExperimentalFeatureEnablementSetResponse"),
    },
    MethodMeta {
        method: "mcpServer/oauth/login",
        params: Some("V2McpServerOauthLoginParams"),
        response: Some("V2McpServerOauthLoginResponse"),
    },
    MethodMeta {
        method: "config/mcpServer/reload",
        params: None,
        response: Some("V2McpServerRefreshResponse"),
    },
    MethodMeta {
        method: "mcpServerStatus/list",
        params: Some("V2ListMcpServerStatusParams"),
        response: Some("V2ListMcpServerStatusResponse"),
    },
    MethodMeta {
        method: "mcpServer/resource/read",
        params: Some("V2McpResourceReadParams"),
        response: Some("V2McpResourceReadResponse"),
    },
    MethodMeta {
        method: "mcpServer/tool/call",
        params: Some("V2McpServerToolCallParams"),
        response: Some("V2McpServerToolCallResponse"),
    },
    MethodMeta {
        method: "windowsSandbox/setupStart",
        params: Some("V2WindowsSandboxSetupStartParams"),
        response: Some("V2WindowsSandboxSetupStartResponse"),
    },
    MethodMeta {
        method: "windowsSandbox/readiness",
        params: None,
        response: Some("V2WindowsSandboxReadinessResponse"),
    },
    MethodMeta {
        method: "account/login/start",
        params: Some("V2LoginAccountParams"),
        response: Some("V2LoginAccountResponse"),
    },
    MethodMeta {
        method: "account/login/cancel",
        params: Some("V2CancelLoginAccountParams"),
        response: Some("V2CancelLoginAccountResponse"),
    },
    MethodMeta {
        method: "account/logout",
        params: None,
        response: Some("V2LogoutAccountResponse"),
    },
    MethodMeta {
        method: "account/rateLimits/read",
        params: Some("V2NullableGetAccountRateLimitsParams"),
        response: Some("V2GetAccountRateLimitsResponse"),
    },
    MethodMeta {
        method: "account/rateLimitResetCredit/consume",
        params: Some("V2ConsumeAccountRateLimitResetCreditParams"),
        response: Some("V2ConsumeAccountRateLimitResetCreditResponse"),
    },
    MethodMeta {
        method: "account/usage/read",
        params: Some("V2NullableGetAccountTokenUsageParams"),
        response: Some("V2GetAccountTokenUsageResponse"),
    },
    MethodMeta {
        method: "account/workspaceMessages/read",
        params: None,
        response: Some("V2GetWorkspaceMessagesResponse"),
    },
    MethodMeta {
        method: "account/sendAddCreditsNudgeEmail",
        params: Some("V2SendAddCreditsNudgeEmailParams"),
        response: Some("V2SendAddCreditsNudgeEmailResponse"),
    },
    MethodMeta {
        method: "feedback/upload",
        params: Some("V2FeedbackUploadParams"),
        response: Some("V2FeedbackUploadResponse"),
    },
    MethodMeta {
        method: "command/exec",
        params: Some("V2CommandExecParams"),
        response: Some("V2CommandExecResponse"),
    },
    MethodMeta {
        method: "command/exec/write",
        params: Some("V2CommandExecWriteParams"),
        response: Some("V2CommandExecWriteResponse"),
    },
    MethodMeta {
        method: "command/exec/terminate",
        params: Some("V2CommandExecTerminateParams"),
        response: Some("V2CommandExecTerminateResponse"),
    },
    MethodMeta {
        method: "command/exec/resize",
        params: Some("V2CommandExecResizeParams"),
        response: Some("V2CommandExecResizeResponse"),
    },
    MethodMeta {
        method: "config/read",
        params: Some("V2ConfigReadParams"),
        response: Some("V2ConfigReadResponse"),
    },
    MethodMeta {
        method: "externalAgentConfig/detect",
        params: Some("V2ExternalAgentConfigDetectParams"),
        response: Some("V2ExternalAgentConfigDetectResponse"),
    },
    MethodMeta {
        method: "externalAgentConfig/import",
        params: Some("V2ExternalAgentConfigImportParams"),
        response: Some("V2ExternalAgentConfigImportResponse"),
    },
    MethodMeta {
        method: "externalAgentConfig/import/recordHistory",
        params: Some("V2ExternalAgentConfigImportHistoryRecordParams"),
        response: Some("V2ExternalAgentConfigImportHistoryRecordResponse"),
    },
    MethodMeta {
        method: "externalAgentConfig/import/readHistories",
        params: None,
        response: Some("V2ExternalAgentConfigImportHistoriesReadResponse"),
    },
    MethodMeta {
        method: "config/value/write",
        params: Some("V2ConfigValueWriteParams"),
        response: Some("V2ConfigWriteResponse"),
    },
    MethodMeta {
        method: "config/batchWrite",
        params: Some("V2ConfigBatchWriteParams"),
        response: Some("V2ConfigWriteResponse"),
    },
    MethodMeta {
        method: "configRequirements/read",
        params: None,
        response: Some("V2ConfigRequirementsReadResponse"),
    },
    MethodMeta {
        method: "account/read",
        params: Some("V2GetAccountParams"),
        response: Some("V2GetAccountResponse"),
    },
    MethodMeta {
        method: "getConversationSummary",
        params: Some("GetConversationSummaryParams"),
        response: Some("GetConversationSummaryResponse"),
    },
    MethodMeta {
        method: "gitDiffToRemote",
        params: Some("GitDiffToRemoteParams"),
        response: Some("GitDiffToRemoteResponse"),
    },
    MethodMeta {
        method: "getAuthStatus",
        params: Some("GetAuthStatusParams"),
        response: Some("GetAuthStatusResponse"),
    },
    MethodMeta {
        method: "fuzzyFileSearch",
        params: Some("FuzzyFileSearchParams"),
        response: Some("FuzzyFileSearchResponse"),
    },
];
pub const CLIENT_NOTIFICATION_METHODS: &[MethodMeta] = &[MethodMeta {
    method: "initialized",
    params: None,
    response: None,
}];
pub const SERVER_REQUEST_METHODS: &[MethodMeta] = &[
    MethodMeta {
        method: "item/commandExecution/requestApproval",
        params: Some("CommandExecutionRequestApprovalParams"),
        response: Some("CommandExecutionRequestApprovalResponse"),
    },
    MethodMeta {
        method: "item/fileChange/requestApproval",
        params: Some("FileChangeRequestApprovalParams"),
        response: Some("FileChangeRequestApprovalResponse"),
    },
    MethodMeta {
        method: "item/tool/requestUserInput",
        params: Some("ToolRequestUserInputParams"),
        response: Some("ToolRequestUserInputResponse"),
    },
    MethodMeta {
        method: "mcpServer/elicitation/request",
        params: Some("McpServerElicitationRequestParams"),
        response: Some("McpServerElicitationRequestResponse"),
    },
    MethodMeta {
        method: "item/permissions/requestApproval",
        params: Some("PermissionsRequestApprovalParams"),
        response: Some("PermissionsRequestApprovalResponse"),
    },
    MethodMeta {
        method: "item/tool/call",
        params: Some("DynamicToolCallParams"),
        response: Some("DynamicToolCallResponse"),
    },
    MethodMeta {
        method: "account/chatgptAuthTokens/refresh",
        params: Some("ChatgptAuthTokensRefreshParams"),
        response: Some("ChatgptAuthTokensRefreshResponse"),
    },
    MethodMeta {
        method: "attestation/generate",
        params: Some("AttestationGenerateParams"),
        response: Some("AttestationGenerateResponse"),
    },
    MethodMeta {
        method: "applyPatchApproval",
        params: Some("ApplyPatchApprovalParams"),
        response: Some("ApplyPatchApprovalResponse"),
    },
    MethodMeta {
        method: "execCommandApproval",
        params: Some("ExecCommandApprovalParams"),
        response: Some("ExecCommandApprovalResponse"),
    },
];
pub const SERVER_NOTIFICATION_METHODS: &[MethodMeta] = &[
    MethodMeta {
        method: "error",
        params: Some("V2ErrorNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/started",
        params: Some("V2ThreadStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/status/changed",
        params: Some("V2ThreadStatusChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/archived",
        params: Some("V2ThreadArchivedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/deleted",
        params: Some("V2ThreadDeletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/unarchived",
        params: Some("V2ThreadUnarchivedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/closed",
        params: Some("V2ThreadClosedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/reverted",
        params: Some("V2ThreadRevertedNotification"),
        response: None,
    },
    MethodMeta {
        method: "skills/changed",
        params: Some("V2SkillsChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/name/updated",
        params: Some("V2ThreadNameUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/attachment/updated",
        params: Some("V2ThreadAttachmentUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/goal/updated",
        params: Some("V2ThreadGoalUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/goal/cleared",
        params: Some("V2ThreadGoalClearedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/queue/changed",
        params: Some("V2ThreadQueueChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "project/changed",
        params: Some("V2ProjectChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/project/updated",
        params: Some("V2ThreadProjectUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/environment/connected",
        params: Some("V2EnvironmentConnectionNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/environment/disconnected",
        params: Some("V2EnvironmentConnectionNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/settings/updated",
        params: Some("V2ThreadSettingsUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/tokenUsage/updated",
        params: Some("V2ThreadTokenUsageUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "turn/started",
        params: Some("V2TurnStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "hook/started",
        params: Some("V2HookStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "turn/completed",
        params: Some("V2TurnCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "hook/completed",
        params: Some("V2HookCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "turn/diff/updated",
        params: Some("V2TurnDiffUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "turn/plan/updated",
        params: Some("V2TurnPlanUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/started",
        params: Some("V2ItemStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/autoApprovalReview/started",
        params: Some("V2ItemGuardianApprovalReviewStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/autoApprovalReview/completed",
        params: Some("V2ItemGuardianApprovalReviewCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "autoApprovalReview/strictReviewRequired",
        params: Some("V2StrictReviewRequiredNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/completed",
        params: Some("V2ItemCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "rawResponseItem/completed",
        params: Some("V2RawResponseItemCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "rawResponse/completed",
        params: Some("V2RawResponseCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/agentMessage/delta",
        params: Some("V2AgentMessageDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/plan/delta",
        params: Some("V2PlanDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "command/exec/outputDelta",
        params: Some("V2CommandExecOutputDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "process/outputDelta",
        params: Some("V2ProcessOutputDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "process/exited",
        params: Some("V2ProcessExitedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/commandExecution/outputDelta",
        params: Some("V2CommandExecutionOutputDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/commandExecution/terminalInteraction",
        params: Some("V2TerminalInteractionNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/fileChange/outputDelta",
        params: Some("V2FileChangeOutputDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/fileChange/patchUpdated",
        params: Some("V2FileChangePatchUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "serverRequest/resolved",
        params: Some("V2ServerRequestResolvedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/mcpToolCall/progress",
        params: Some("V2McpToolCallProgressNotification"),
        response: None,
    },
    MethodMeta {
        method: "mcpServer/oauthLogin/completed",
        params: Some("V2McpServerOauthLoginCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "mcpServer/startupStatus/updated",
        params: Some("V2McpServerStatusUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "mcpServer/event/stream/notification",
        params: Some("V2McpServerEventStreamNotification"),
        response: None,
    },
    MethodMeta {
        method: "account/updated",
        params: Some("V2AccountUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "account/gatewayOAuth/changed",
        params: Some("V2GatewayOAuthChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "account/rateLimits/updated",
        params: Some("V2AccountRateLimitsUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "app/list/updated",
        params: Some("V2AppListUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "remoteControl/status/changed",
        params: Some("V2RemoteControlStatusChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "externalAgentConfig/import/progress",
        params: Some("V2ExternalAgentConfigImportProgressNotification"),
        response: None,
    },
    MethodMeta {
        method: "externalAgentConfig/import/completed",
        params: Some("V2ExternalAgentConfigImportCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "fs/changed",
        params: Some("V2FsChangedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/reasoning/summaryTextDelta",
        params: Some("V2ReasoningSummaryTextDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/reasoning/summaryPartAdded",
        params: Some("V2ReasoningSummaryPartAddedNotification"),
        response: None,
    },
    MethodMeta {
        method: "item/reasoning/textDelta",
        params: Some("V2ReasoningTextDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/compacted",
        params: Some("V2ContextCompactedNotification"),
        response: None,
    },
    MethodMeta {
        method: "model/rerouted",
        params: Some("V2ModelReroutedNotification"),
        response: None,
    },
    MethodMeta {
        method: "model/verification",
        params: Some("V2ModelVerificationNotification"),
        response: None,
    },
    MethodMeta {
        method: "modelProvider/authRecoveryStarted",
        params: Some("V2AuthRecoveryNotification"),
        response: None,
    },
    MethodMeta {
        method: "modelProvider/authRecoveryCompleted",
        params: Some("V2AuthRecoveryNotification"),
        response: None,
    },
    MethodMeta {
        method: "turn/moderationMetadata",
        params: Some("V2TurnModerationMetadataNotification"),
        response: None,
    },
    MethodMeta {
        method: "model/safetyBuffering/updated",
        params: Some("V2ModelSafetyBufferingUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "warning",
        params: Some("V2WarningNotification"),
        response: None,
    },
    MethodMeta {
        method: "guardianWarning",
        params: Some("V2GuardianWarningNotification"),
        response: None,
    },
    MethodMeta {
        method: "deprecationNotice",
        params: Some("V2DeprecationNoticeNotification"),
        response: None,
    },
    MethodMeta {
        method: "configWarning",
        params: Some("V2ConfigWarningNotification"),
        response: None,
    },
    MethodMeta {
        method: "fuzzyFileSearch/sessionUpdated",
        params: Some("FuzzyFileSearchSessionUpdatedNotification"),
        response: None,
    },
    MethodMeta {
        method: "fuzzyFileSearch/sessionCompleted",
        params: Some("FuzzyFileSearchSessionCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/started",
        params: Some("V2ThreadRealtimeStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/itemAdded",
        params: Some("V2ThreadRealtimeItemAddedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/item/started",
        params: Some("V2ThreadRealtimeItemStartedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/item/transcript/delta",
        params: Some("V2ThreadRealtimeItemTranscriptDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/item/completed",
        params: Some("V2ThreadRealtimeItemCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/transcript/delta",
        params: Some("V2ThreadRealtimeTranscriptDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/transcript/done",
        params: Some("V2ThreadRealtimeTranscriptDoneNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/outputAudio/delta",
        params: Some("V2ThreadRealtimeOutputAudioDeltaNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/sdp",
        params: Some("V2ThreadRealtimeSdpNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/error",
        params: Some("V2ThreadRealtimeErrorNotification"),
        response: None,
    },
    MethodMeta {
        method: "thread/realtime/closed",
        params: Some("V2ThreadRealtimeClosedNotification"),
        response: None,
    },
    MethodMeta {
        method: "windows/worldWritableWarning",
        params: Some("V2WindowsWorldWritableWarningNotification"),
        response: None,
    },
    MethodMeta {
        method: "windowsSandbox/setupCompleted",
        params: Some("V2WindowsSandboxSetupCompletedNotification"),
        response: None,
    },
    MethodMeta {
        method: "account/login/completed",
        params: Some("V2AccountLoginCompletedNotification"),
        response: None,
    },
];
pub fn method_meta<'a>(methods: &'a [MethodMeta], method: &str) -> Option<&'a MethodMeta> {
    methods.iter().find(|entry| entry.method == method)
}
