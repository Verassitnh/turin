//! Usage reads for the live usage page via [`Commands::usage_summary`].

use turin_contracts::usage::{UsageSummary, UsageSummaryInput};

use crate::app_runtime::write::commands::{CommandError, Commands};

pub async fn usage_summary(
    commands: &Commands,
    input: &UsageSummaryInput,
) -> Result<UsageSummary, CommandError> {
    commands.usage_summary(input).await
}
