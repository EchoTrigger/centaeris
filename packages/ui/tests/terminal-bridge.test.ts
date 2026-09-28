import { expect, test, vi } from "vitest";
import { readFileSync } from "node:fs";
import { parseTerminal, writeTerminal } from "../src/lib/terminalBridge";
import { invokeHost } from "../src/host/hostBridge";
vi.mock("../src/host/hostBridge", () => ({ invokeHost: vi.fn() }));
test("terminal schema matches the Rust wire fixture and rejects unknown fields", () => {
	const sample = JSON.parse(
		readFileSync(
			new URL("../../runtime/generated/terminal-samples.json", import.meta.url),
			"utf8",
		),
	);
	expect(parseTerminal(sample)).toEqual(sample);
	expect(() => parseTerminal({ ...sample, unknown: true })).toThrow();
	expect(() => parseTerminal({ ...sample, state: "ready" })).toThrow();
});
test("failed terminal input is sent once and never retried", async () => {
	vi.mocked(invokeHost).mockRejectedValueOnce(Error("disconnected"));
	await expect(
		writeTerminal(
			{ terminalId: "t", serviceInstanceId: "s" },
			new TextEncoder().encode("exit\r"),
		),
	).rejects.toThrow("disconnected");
	expect(invokeHost).toHaveBeenCalledTimes(1);
	expect(invokeHost).toHaveBeenCalledWith("terminal_manage", {
		request: {
			action: "write",
			target: { terminalId: "t", serviceInstanceId: "s" },
			dataBase64: "ZXhpdA0=",
		},
	});
});
