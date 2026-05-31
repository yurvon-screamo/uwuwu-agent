import { tool } from "@opencode-ai/plugin";

const BASE_URL = "http://127.0.0.1:8420";

async function apiRequest(
    path: string,
    method: string = "GET",
    body?: unknown,
): Promise<string> {
    const opts: RequestInit = {
        method,
        headers: { "Content-Type": "application/json" },
    };
    if (body !== undefined) {
        opts.body = JSON.stringify(body);
    }
    const res = await fetch(`${BASE_URL}${path}`, opts);
    const data = await res.json();
    return JSON.stringify(data, null, 2);
}

export const recall = tool({
    description:
        "Retrieve relevant context from memory by query. Returns found memories and conversations.",
    args: {
        query: tool.schema
            .string()
            .describe("Query text to search for relevant context in memory"),
        session_key: tool.schema
            .string()
            .describe("Unique key of the current session"),
        user_id: tool.schema.string().optional().describe("User ID (optional)"),
    },
    async execute(args) {
        const body: Record<string, string> = {
            query: args.query,
            session_key: args.session_key,
        };
        if (args.user_id) body.user_id = args.user_id;
        return apiRequest("/recall", "POST", body);
    },
});

export const capture = tool({
    description:
        "Save a question-answer pair to memory. Use at the end of a session or after significant findings.",
    args: {
        user_content: tool.schema
            .string()
            .describe("User message or task description"),
        assistant_content: tool.schema
            .string()
            .describe("Assistant response or work results"),
        session_key: tool.schema
            .string()
            .describe("Unique key of the current session"),
        session_id: tool.schema
            .string()
            .optional()
            .describe("Session ID (optional)"),
        user_id: tool.schema.string().optional().describe("User ID (optional)"),
    },
    async execute(args) {
        const body: Record<string, string> = {
            user_content: args.user_content,
            assistant_content: args.assistant_content,
            session_key: args.session_key,
        };
        if (args.session_id) body.session_id = args.session_id;
        if (args.user_id) body.user_id = args.user_id;
        return apiRequest("/capture", "POST", body);
    },
});

export const search_memories = tool({
    description:
        "Full-text + vector search over extracted memories (L1). Use to find insights, conclusions, patterns.",
    args: {
        query: tool.schema.string().describe("Search query for memories"),
        limit: tool.schema
            .number()
            .optional()
            .describe("Maximum number of results (default 10)"),
        type: tool.schema.string().optional().describe("Filter by memory type"),
        scene: tool.schema
            .string()
            .optional()
            .describe("Filter by scene (context)"),
    },
    async execute(args) {
        const body: Record<string, unknown> = { query: args.query };
        if (args.limit !== undefined) body.limit = args.limit;
        if (args.type) body.type = args.type;
        if (args.scene) body.scene = args.scene;
        return apiRequest("/search/memories", "POST", body);
    },
});

export const search_conversations = tool({
    description:
        "Search over raw conversations (L0). Use to find specific conversations.",
    args: {
        query: tool.schema.string().describe("Search query for conversations"),
        limit: tool.schema
            .number()
            .optional()
            .describe("Maximum number of results (default 10)"),
        session_key: tool.schema
            .string()
            .optional()
            .describe("Filter by session key"),
    },
    async execute(args) {
        const body: Record<string, unknown> = { query: args.query };
        if (args.limit !== undefined) body.limit = args.limit;
        if (args.session_key) body.session_key = args.session_key;
        return apiRequest("/search/conversations", "POST", body);
    },
});

export const session_end = tool({
    description:
        "End the memory session and flush buffers. Call at the end of a session.",
    args: {
        session_key: tool.schema
            .string()
            .describe("Unique key of the session to end"),
        user_id: tool.schema.string().optional().describe("User ID (optional)"),
    },
    async execute(args) {
        const body: Record<string, string> = {
            session_key: args.session_key,
        };
        if (args.user_id) body.user_id = args.user_id;
        return apiRequest("/session/end", "POST", body);
    },
});

export const seed = tool({
    description:
        "Batch upload of historical conversations into memory with subsequent processing.",
    args: {
        data: tool.schema
            .object({
                sessions: tool.schema.array(
                    tool.schema.object({
                        sessionKey: tool.schema.string(),
                        conversations: tool.schema.array(
                            tool.schema.array(
                                tool.schema.object({
                                    role: tool.schema.string(),
                                    content: tool.schema.string(),
                                }),
                            ),
                        ),
                    }),
                ),
            })
            .describe("Session data to upload into memory"),
        session_key: tool.schema
            .string()
            .optional()
            .describe("Fallback session key"),
        strict_round_role: tool.schema
            .boolean()
            .optional()
            .describe("Strict role validation (default false)"),
        auto_fill_timestamps: tool.schema
            .boolean()
            .optional()
            .describe("Auto-fill timestamps (default true)"),
    },
    async execute(args) {
        const body: Record<string, unknown> = { data: args.data };
        if (args.session_key) body.session_key = args.session_key;
        if (args.strict_round_role !== undefined)
            body.strict_round_role = args.strict_round_role;
        if (args.auto_fill_timestamps !== undefined)
            body.auto_fill_timestamps = args.auto_fill_timestamps;
        return apiRequest("/seed", "POST", body);
    },
});
