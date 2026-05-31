import { tool } from "@opencode-ai/plugin";

export const current = tool({
    description: "Get the current date and time from the PC's local timezone.",
    args: {},
    async execute() {
        const now = new Date();
        const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
        const offsetMin = -now.getTimezoneOffset();
        const sign = offsetMin >= 0 ? "+" : "-";
        const h = String(Math.floor(Math.abs(offsetMin) / 60)).padStart(2, "0");
        const m = String(Math.abs(offsetMin) % 60).padStart(2, "0");
        const local = now
            .toLocaleString("sv-SE", {
                year: "numeric",
                month: "2-digit",
                day: "2-digit",
                hour: "2-digit",
                minute: "2-digit",
                second: "2-digit",
                hour12: false,
            })
            .replace(" ", "T");
        return `${local}${sign}${h}:${m} (${tz})`;
    },
});
