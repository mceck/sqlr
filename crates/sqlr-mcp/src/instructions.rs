//! The initialize instructions: a short overview generated in code. Workflows live in the
//! sqlr skills; argument details live in the tool schemas.

use crate::registry::TOOLS;

/// The command that installs the sqlr skills into an agent.
pub const SKILLS_COMMAND: &str = "npx skills add mceck/sqlr";

/// The instructions the initialize response carries. They hold descriptions only: no
/// secret, Connection value or environment-specific data.
pub fn instructions() -> String {
    format!(
        "sqlr is a desktop database client. This MCP server runs only while the app runs \
         and serves {count} tools.\n\
         \n\
         - Exposure: you see only the Connections the person exposed to agents as Read or \
         Write; the person decides exposure, and you cannot change it. Start with \
         config_list(target=connections).\n\
         - Approvals: statements the Connection profile asks to confirm, and every \
         configuration or Snippet delete, wait for the person in an Approval dialog; denial \
         and timeout are decisions, not faults. Safety flags only tighten: you cannot relax \
         a profile, a tighten-only setting or a transport-security field.\n\
         - Connection values and secrets are write-only: you can set them, never read them \
         back.\n\
         - Workflows live in the sqlr skills (sqlr-configure, sqlr-create-connection, \
         sqlr-edit-user-query, sqlr-query-database, sqlr-connect-agent). Install them with \
         `{SKILLS_COMMAND}`; the Integrated terminal's workspace already has them.",
        count = TOOLS.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_name_the_skills_command_and_no_guide_tool() {
        let text = instructions();
        assert!(text.contains(SKILLS_COMMAND));
        assert!(!text.contains("get_guide"));
    }
}
