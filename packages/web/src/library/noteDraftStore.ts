import { openDB, type DBSchema } from "idb";

export type NoteDocument = { displayName: string; markdown: string };
export type NoteSaveOperation = {
  saveId: string; sequence: number; noteId: string | null; baseGeneration: number;
  document: NoteDocument; parentFolderId: string;
};
export type NoteDraft = {
  schema: 1; key: string; userId: string; clientDraftId: string; editInstanceId: string;
  noteId: string | null; parentFolderId: string; baseGeneration: number; sequence: number;
  document: NoteDocument; pending: NoteSaveOperation | null; updatedAt: number;
};
export interface NoteDraftStore {
  put(record: NoteDraft): Promise<void>;
  list(userId: string, targetId: string): Promise<NoteDraft[]>;
  deleteIfSequence(key: string, sequence: number): Promise<void>;
}
interface DraftDatabase extends DBSchema {
  drafts: { key: string; value: NoteDraft; indexes: { note: [string, string]; clientDraft: [string, string] } };
}

export function createNoteDraftStore(name = "centaeris.noteDrafts.v1"): NoteDraftStore {
  const database = openDB<DraftDatabase>(name, 1, {
    upgrade(db) {
      const store = db.createObjectStore("drafts", { keyPath: "key" });
      store.createIndex("note", ["userId", "noteId"]);
      store.createIndex("clientDraft", ["userId", "clientDraftId"]);
    },
    blocking(_current, _blocked, _event) { void database.then(db => db.close()); },
  });
  function validate(record: NoteDraft) {
    const exact = (value: object, fields: string[]) => value && Object.keys(value).length === fields.length && fields.every(key => Object.hasOwn(value, key));
    const text = (value: NoteDocument) => exact(value, ["displayName", "markdown"]) && typeof value.displayName === "string" && typeof value.markdown === "string";
    const fields = ["schema", "key", "userId", "clientDraftId", "editInstanceId", "noteId", "parentFolderId", "baseGeneration", "sequence", "document", "pending", "updatedAt"];
    if (!exact(record, fields) || record.schema !== 1 || !record.userId || !record.clientDraftId || !record.editInstanceId
        || record.key !== JSON.stringify([record.userId, record.clientDraftId, record.editInstanceId])
        || !Number.isSafeInteger(record.sequence) || record.sequence < 0 || !Number.isSafeInteger(record.baseGeneration) || record.baseGeneration < 0
        || !Number.isFinite(record.updatedAt) || typeof record.parentFolderId !== "string"
        || (record.noteId !== null && (typeof record.noteId !== "string" || !record.noteId)) || !text(record.document)) {
      throw new Error("note_draft_schema_invalid");
    }
    const pending = record.pending;
    if (pending !== null && (!exact(pending, ["saveId", "sequence", "noteId", "baseGeneration", "document", "parentFolderId"])
        || !/^[0-9a-f]{32}$/.test(pending.saveId) || !Number.isSafeInteger(pending.sequence) || pending.sequence < 0 || pending.sequence > record.sequence
        || pending.baseGeneration !== record.baseGeneration || pending.noteId !== record.noteId
        || pending.parentFolderId !== record.parentFolderId || !text(pending.document))) throw new Error("note_draft_schema_invalid");
  }
  return {
    async put(record) {
      validate(record);
      const db = await database;
      const tx = db.transaction("drafts", "readwrite");
      await Promise.all([tx.store.put(structuredClone(record)), tx.done]);
    },
    async list(userId, targetId) {
      const db = await database;
      const groups = await Promise.all([db.getAllFromIndex("drafts", "note", [userId, targetId]), db.getAllFromIndex("drafts", "clientDraft", [userId, targetId])]);
      const records = [...new Map(groups.flat().map(record => [record.key, record])).values()];
      records.forEach(validate);
      return records.sort((a, b) => b.updatedAt - a.updatedAt);
    },
    async deleteIfSequence(key, sequence) {
      const db = await database;
      const tx = db.transaction("drafts", "readwrite");
      const record = await tx.store.get(key);
      if (record?.sequence === sequence) await tx.store.delete(key);
      await tx.done;
    },
  };
}
