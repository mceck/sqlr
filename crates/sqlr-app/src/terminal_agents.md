# sqlr terminal workspace

This folder is the working directory of sqlr's integrated terminal, inside sqlr's
configuration directory. Files you create here persist across terminal restarts and
sqlr launches.

## Reaching sqlr

Agents reach sqlr through its MCP server while the app is open. Install this agent once
from **Settings → Agents** in sqlr (press **Install**, then restart the agent); sqlr keeps
the installed entry current when its token or port changes.

## Skills

Use the `sqlr-*` skills in `.agents/skills` (also linked from `.claude/skills`) for any
database work in sqlr. When the sqlr tools are missing or failing, start with
`sqlr-connect-agent`. sqlr keeps its `sqlr-*` skills current; skills you add beside them
are yours.

## This file

This file belongs to you: sqlr creates it only when it is missing and never rewrites it,
so add your own instructions here.
