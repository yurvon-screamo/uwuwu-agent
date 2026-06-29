import { tool } from "@opencode-ai/plugin";
import { readFile } from "fs/promises";
import { basename } from "path";

const OLLAMA_URL = "http://127.0.0.1:11434";
const MODEL = "qwen3.5:2b";

interface OllamaChatResponse {
    message?: { content?: string };
    error?: string;
}

export const ask = tool({
    description:
        "Answer a question about an image using a local vision LLM via Ollama " +
        `(model: ${MODEL}, non-thinking mode). Pass an absolute path to the ` +
        "image file and a natural-language question. Returns the model's text answer.",
    args: {
        image_path: tool.schema
            .string()
            .describe("Absolute path to the image file on disk"),
        question: tool.schema
            .string()
            .describe("Natural-language question about the image"),
    },
    async execute(args) {
        const buffer = await readFile(args.image_path);
        const base64 = buffer.toString("base64");

        const res = await fetch(`${OLLAMA_URL}/api/chat`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
                model: MODEL,
                stream: false,
                think: false,
                messages: [
                    {
                        role: "user",
                        content: args.question,
                        images: [base64],
                    },
                ],
            }),
        });

        const data = (await res.json()) as OllamaChatResponse;
        if (!res.ok || data.error) {
            throw new Error(
                `Ollama error (${res.status}): ${data.error ?? res.statusText}`,
            );
        }
        const content = data.message?.content?.trim();
        if (!content) {
            throw new Error(
                `Ollama returned empty answer for ${basename(args.image_path)}`,
            );
        }
        return content;
    },
});
