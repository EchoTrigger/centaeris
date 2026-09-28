import { expect, test } from "vitest";
import { SessionCatalogClient } from "../src/lib/sessionCatalogClient";

test("initial catalog is bounded and subsequent discovery asks for changes", async () => {
	const requests: any[] = [];
	const client = new SessionCatalogClient(async (request) => {
		requests.push(request);
		return {
			items: [],
			deletedIds: [],
			nextCursor: null,
			revision: "rev",
			reset: false,
		};
	});
	await client.initialize();
	expect(requests.map((r) => r.mode)).toEqual(["changes", "pinned", "recent"]);
	requests.length = 0;
	await client.sync();
	expect(requests).toEqual([{ mode: "changes", cursor: "rev", limit: 100 }]);
});

test("workspace pagination replaces a stale cursor and does not duplicate entries", async () => {
	const requests: any[] = [];
	let page = 0;
	const client = new SessionCatalogClient(async (request) => {
		requests.push(request);
		if (request.mode === "changes")
			return {
				items: [],
				deletedIds: [],
				revision: "r",
				nextCursor: null,
				reset: false,
			};
		if (request.cwd) {
			page++;
			if (page === 2)
				return {
					items: [],
					deletedIds: [],
					revision: "r2",
					nextCursor: null,
					reset: true,
				};
			return {
				items: [
					{
						id: "a",
						title: "a",
						cwd: "/a",
						sessionKind: "main",
						updatedAt: page,
						messageCount: 0,
						activityState: "inactive",
					},
				],
				deletedIds: [],
				revision: "r",
				nextCursor: page === 1 ? "next" : null,
				reset: false,
			};
		}
		return {
			items: [],
			deletedIds: [],
			revision: "r",
			nextCursor: null,
			reset: false,
		};
	});
	await client.initialize();
	await client.expand("/a");
	await client.more("/a");
	expect(client.items().map((i) => i.id)).toEqual(["a"]);
	expect(requests.filter((r) => r.cwd).map((r) => r.cursor)).toEqual([
		undefined,
		"next",
		undefined,
	]);
});
