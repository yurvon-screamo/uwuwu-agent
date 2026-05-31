if (Test-Path ~/.agents/skills) { Remove-Item -Force -Recurse ~/.agents/skills }
if (Test-Path ~/.config/opencode/agent) { Remove-Item -Force -Recurse ~/.config/opencode/agent }
if (Test-Path ~/.config/opencode/tools) { Remove-Item -Force -Recurse ~/.config/opencode/tools }

Copy-Item -Force -Recurse skills  ~/.agents/skills
Copy-Item -Force AGENTS.md ~/.agents/AGENTS.md

Copy-Item -Force -Recurse agent  ~/.config/opencode/agent
Copy-Item -Force -Recurse tools  ~/.config/opencode/tools

Copy-Item -Force AGENTS.md ~/.config/opencode/AGENTS.md
Copy-Item -Force opencode.json ~/.config/opencode/opencode.json
Copy-Item -Force dcp.jsonc ~/.config/opencode/dcp.jsonc
Copy-Item -Force package.json ~/.config/opencode/package.json
