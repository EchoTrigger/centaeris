import type { SessionItem } from "../src/lib/chatBridge";
import type {
	CatalogRequest,
	CatalogPage,
} from "../src/lib/sessionCatalogClient";
/** Adapt lifecycle fixtures to the paged Host contract; pagination has separate tests. */
export function catalogFixture(list: () => Promise<SessionItem[]>) {
	let ids = new Set<string>();
	return async (request: CatalogRequest): Promise<CatalogPage> => {
		const empty = {
			items: [],
			deletedIds: [],
			revision: "fixture-revision",
			nextCursor: null,
			reset: false,
		};
		if (request.mode === "changes" && !request.cursor) {
			ids = new Set();
			return empty;
		}
		if (request.mode === "pinned" || request.cwd) return empty;
		const items = await list();
		if (request.mode === "lookup")
			return {
				...empty,
				items: items.filter((i) => i.id === request.sessionId),
			};
		const next = new Set(items.map((i) => i.id));
		const deletedIds = [...ids].filter((id) => !next.has(id));
		ids = next;
		return { ...empty, items, deletedIds };
	};
}
