import { tool } from "@opencode-ai/plugin";
import { execSync } from "child_process";
import { writeFileSync, mkdirSync } from "fs";
import { join } from "path";

const WIKI_ROOT = process.env.WIKI_ROOT || "D:\\uwuwu_wiki";
const REQUESTS_DIR = join(WIKI_ROOT, "wiki-cli", ".requests");

export const wiki_search = tool({
    description:
        "Search uwuwu_wiki for relevant articles. Returns full article text for top 3 matches above threshold 0.3. Use 'experience' for howto/tech articles, 'access' for credentials/stands/topology.",
    args: {
        query: tool.schema
            .string()
            .describe("Search query — be descriptive for better results"),
        doc_type: tool.schema
            .string()
            .optional()
            .describe("'experience' (default) or 'access'"),
    },
    async execute(args) {
        const type = args.doc_type || "experience";
        try {
            const escapedQuery = args.query.replace(/"/g, '\\"');
            return execSync(`wiki-cli search ${type} "${escapedQuery}"`, {
                encoding: "utf-8",
                timeout: 120000,
                maxBuffer: 50 * 1024 * 1024,
            });
        } catch (e) {
            return `Search failed: ${e}`;
        }
    },
});

export const wiki_request = tool({
    description:
        "Create a change request for uwuwu_wiki (create/update/delete article). Saved to .requests/ for manual review — does NOT modify wiki directly.",
    args: {
        action: tool.schema
            .string()
            .describe("Action: create, update, or delete"),
        content: tool.schema
            .string()
            .describe("Article content (create/update) or target filepath (delete)"),
        reason: tool.schema
            .string()
            .describe("Why this change is needed"),
    },
    async execute(args) {
        mkdirSync(REQUESTS_DIR, { recursive: true });

        const now = new Date();
        const ts = now
            .toISOString()
            .replace(/[:.]/g, "")
            .slice(0, 15);
        const slug = args.reason
            .toLowerCase()
            .replace(/[^a-z0-9-]/g, "-")
            .replace(/-+/g, "-")
            .slice(0, 60);
        const filename = `${ts}_${args.action}_${slug}.md`;
        const filepath = join(REQUESTS_DIR, filename);

        const body = `---\ntype: ${args.action}\nreason: ${args.reason}\ncreated: ${now.toISOString()}\n---\n\n${args.content}\n`;

        writeFileSync(filepath, body, "utf-8");
        return `Request saved: ${filepath}`;
    },
});
