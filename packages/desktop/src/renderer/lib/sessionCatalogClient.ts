export type CatalogRequest = {
	mode: "recent" | "pinned" | "changes" | "lookup";
	cwd?: string;
	cursor?: string;
	sessionId?: string;
	limit?: number;
};
export type CatalogPage = {
	items: import("./chatBridge").SessionItem[];
	deletedIds: string[];
	revision: string;
	nextCursor: string | null;
	reset: boolean;
};
import type { SessionItem } from "./chatBridge";
type Scope = {
	mode: "recent" | "pinned";
	cwd?: string;
	ids: Set<string>;
	next: string | null;
	capacity: number;
};

/** Per-view catalog. No process-global cursor, and all requests share one queue. */
export class SessionCatalogClient {
	private rows = new Map<string, SessionItem>();
	private scopes = new Map<string, Scope>();
	private revision: string | undefined;
	private queue: Promise<unknown> = Promise.resolve();
	constructor(
		private readonly request: (request: CatalogRequest) => Promise<CatalogPage>,
	) {}
	private serial<T>(work: () => Promise<T>): Promise<T> {
		const next = this.queue.then(work, work);
		this.queue = next.catch(() => undefined);
		return next;
	}
	items(): SessionItem[] {
		return [...this.rows.values()];
	}
	hasMore(root: string): boolean {
		return Boolean(this.scopes.get(root)?.next);
	}
	private merge(page: CatalogPage) {
		for (const id of page.deletedIds) {
			this.rows.delete(id);
			for (const scope of this.scopes.values()) scope.ids.delete(id);
		}
		for (const item of page.items) this.rows.set(item.id, item);
	}
	private async page(key: string, append: boolean) {
		const scope = this.scopes.get(key)!;
		let page = await this.request({
			mode: scope.mode,
			cwd: scope.cwd,
			limit: 50,
			...(append && scope.next ? { cursor: scope.next } : {}),
		});
		if (page.reset) {
			page = await this.request({
				mode: scope.mode,
				cwd: scope.cwd,
				limit: 50,
			});
			append = false;
		}
		if (page.reset) throw new Error("Catalog changed while loading; try again");
		if (!append) scope.ids.clear();
		for (const item of page.items) scope.ids.add(item.id);
		scope.next = page.nextCursor;
		scope.capacity = Math.max(50, scope.ids.size);
		this.merge(page);
	}
	private async initial() {
		// Start observation before reading pages so concurrent changes cannot be lost.
		this.revision = (
			await this.request({ mode: "changes", limit: 100 })
		).revision;
		const expanded = [...this.scopes.values()].flatMap((scope) =>
			scope.cwd ? [scope.cwd] : [],
		);
		this.rows.clear();
		this.scopes.clear();
		this.scopes.set("pinned", {
			mode: "pinned",
			ids: new Set(),
			next: null,
			capacity: 50,
		});
		this.scopes.set("recent", {
			mode: "recent",
			ids: new Set(),
			next: null,
			capacity: 50,
		});
		await Promise.all([this.page("pinned", false), this.page("recent", false)]);
		for (const root of expanded) {
			this.scopes.set(root, {
				mode: "recent",
				cwd: root,
				ids: new Set(),
				next: null,
				capacity: 50,
			});
			await this.page(root, false);
		}
		return this.items();
	}
	initialize() {
		return this.serial(() => this.initial());
	}
	sync() {
		return this.serial(async () => {
			if (!this.revision) return this.initial();
			const page = await this.request({
				mode: "changes",
				cursor: this.revision,
				limit: 100,
			});
			if (page.reset) return this.initial();
			this.merge(page);
			this.revision = page.revision;
			// Keep only bounded loaded windows. Explicit pagination enlarges a window.
			for (const [key, scope] of this.scopes) {
				const candidates = this.items().filter(
					(i) =>
						i.sessionKind === "main" &&
						Boolean(i.isPinned) === (scope.mode === "pinned") &&
						(!scope.cwd || normalize(i.cwd ?? "") === normalize(scope.cwd)),
				);
				candidates.sort((a, b) =>
					scope.mode === "pinned"
						? (a.sortOrder ?? 0) - (b.sortOrder ?? 0) ||
							a.id.localeCompare(b.id)
						: (b.updatedAt ?? 0) - (a.updatedAt ?? 0) ||
							a.id.localeCompare(b.id),
				);
				if (candidates.length > scope.capacity && !scope.next) {
					await this.page(key, false);
				} else {
					scope.ids = new Set(
						candidates.slice(0, scope.capacity).map((i) => i.id),
					);
				}
			}
			const retained = new Set(
				[...this.scopes.values()].flatMap((s) => [...s.ids]),
			);
			for (const id of this.rows.keys())
				if (!retained.has(id)) this.rows.delete(id);
			return this.items();
		});
	}
	expand(root: string) {
		return this.serial(async () => {
			if (!this.scopes.has(root)) {
				this.scopes.set(root, {
					mode: "recent",
					cwd: root,
					ids: new Set(),
					next: null,
					capacity: 50,
				});
				try {
					await this.page(root, false);
				} catch (error) {
					this.scopes.delete(root);
					throw error;
				}
			}
			return this.items();
		});
	}
	more(root: string) {
		return this.serial(async () => {
			if (this.scopes.get(root)?.next) await this.page(root, true);
			return this.items();
		});
	}
}
const normalize = (value: string) => {
	const path = value
		.replace(/^\\\\\?\\/, "")
		.replace(/\\/g, "/")
		.replace(/\/+$/, "");
	return /^[a-z]:/i.test(path) || path.startsWith("//")
		? path.toLowerCase()
		: path;
};
